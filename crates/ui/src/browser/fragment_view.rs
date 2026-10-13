//! Visualization mode of a Browser tab: a Codex HTML fragment rendered in the
//! tab's existing native page (see [`super::fragment`] for the sandbox).
use super::fragment::{self, FragmentError, FragmentTheme};
use super::{BrowserEvent, BrowserSurface};
use crate::files::client::WorkspaceFilesClient;
use crate::{icons, surface_chrome, theme::Theme};
use gpui::{AnyElement, Context, IntoElement, SharedString, Task, Window, div, prelude::*, px};
use std::path::PathBuf;
use zeron_proto::WorkspaceTarget;

/// What to render. `path` is checkout-relative and already jailed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FragmentSource {
    pub path: String,
    pub title: String,
    pub wide: bool,
    /// The checkout root when the chat runs on this device; enables Reveal in
    /// Finder and opening the `.preview.html` export in the system browser.
    pub local_root: Option<PathBuf>,
}

impl FragmentSource {
    pub fn local_path(&self) -> Option<PathBuf> {
        Some(self.local_root.as_ref()?.join(&self.path))
    }

    /// The standalone export beside the fragment, if it exists locally.
    pub fn local_preview(&self) -> Option<PathBuf> {
        let preview = zeron_proto::visualize::preview_sibling(&self.path)?;
        let path = self.local_root.as_ref()?.join(preview);
        path.is_file().then_some(path)
    }
}

fn render_key(cx: &gpui::App) -> (FragmentTheme, bool) {
    (
        FragmentTheme::from_theme(Theme::of(cx)),
        crate::motion::reduced_motion(cx),
    )
}

/// The file URL the system browser opens for a local export.
pub fn file_url(path: &std::path::Path) -> Option<String> {
    url::Url::from_file_path(path).ok().map(String::from)
}

pub struct FragmentView {
    pub source: FragmentSource,
    /// The validated fragment text, once read.
    pub html: Option<String>,
    pub error: Option<FragmentError>,
    /// A non-fatal message shown above the visualization.
    pub notice: Option<String>,
    pub loading: bool,
    reader: Option<(WorkspaceFilesClient, WorkspaceTarget)>,
    /// Theme and motion the native page was last rendered with; a theme
    /// notification that changes neither keeps the page (and its state).
    rendered: Option<(FragmentTheme, bool)>,
    generation: u64,
    _task: Option<Task<()>>,
    _theme: Option<gpui::Subscription>,
}

impl BrowserSurface {
    pub fn fragment(&self) -> Option<&FragmentView> {
        self.fragment.as_ref()
    }

    /// Switch this tab to a visualization and read it through the workspace
    /// RPC (checkout-jailed; also works for chats on another device).
    pub fn open_fragment(
        &mut self,
        source: FragmentSource,
        reader: Option<(WorkspaceFilesClient, WorkspaceTarget)>,
        notice: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let theme = cx.observe_global_in::<Theme>(window, |this, window, cx| {
            let key = Some(render_key(cx));
            if this
                .fragment
                .as_ref()
                .is_some_and(|view| view.html.is_some() && view.rendered != key)
            {
                this.show_fragment_document(window, cx);
            }
        });
        self.page = super::model::PageState {
            title: source.title.clone(),
            ..Default::default()
        };
        self.fragment = Some(FragmentView {
            source,
            html: None,
            error: None,
            notice,
            loading: false,
            reader,
            rendered: None,
            generation: 0,
            _task: None,
            _theme: Some(theme),
        });
        self.reload_fragment(window, cx);
    }

    pub fn set_fragment_notice(&mut self, notice: Option<String>, cx: &mut Context<Self>) {
        if let Some(view) = &mut self.fragment {
            view.notice = notice;
            cx.notify();
        }
    }

    pub fn reload_fragment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = &mut self.fragment else {
            return;
        };
        view.generation += 1;
        view.error = None;
        let Some((client, target)) = view.reader.clone() else {
            view.loading = false;
            view.error = Some(FragmentError::Unavailable {
                detail: "This chat’s workspace isn’t connected.".into(),
            });
            cx.emit(BrowserEvent::Changed);
            cx.notify();
            return;
        };
        view.loading = true;
        let generation = view.generation;
        let path = view.source.path.clone();
        view._task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = client
                .read_file(zeron_proto::ReadWorkspaceFileRequest {
                    target,
                    path: path.clone(),
                })
                .await;
            let result = fragment::fragment_text(result, &path);
            let _ = this.update_in(cx, |this, window, cx| {
                this.fragment_loaded(generation, result, window, cx)
            });
        }));
        cx.notify();
    }

    pub(super) fn fragment_loaded(
        &mut self,
        generation: u64,
        result: Result<String, FragmentError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = &mut self.fragment else {
            return;
        };
        if view.generation != generation {
            return;
        }
        view.loading = false;
        match result {
            Ok(html) => {
                view.html = Some(html);
                view.error = None;
                self.show_fragment_document(window, cx);
            }
            Err(error) => {
                view.html = None;
                view.error = Some(error);
            }
        }
        cx.emit(BrowserEvent::Changed);
        cx.notify();
    }

    /// The host document for the current theme and motion preference.
    pub fn fragment_document(&self, cx: &gpui::App) -> Option<String> {
        let view = self.fragment.as_ref()?;
        let html = view.html.as_deref()?;
        Some(fragment::host_document(
            html,
            &view.source.title,
            view.source.wide,
            &FragmentTheme::from_theme(Theme::of(cx)),
            crate::motion::reduced_motion(cx),
        ))
    }

    fn show_fragment_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(document) = self.fragment_document(cx) else {
            return;
        };
        let key = render_key(cx);
        if let Some(view) = &mut self.fragment {
            view.rendered = Some(key);
        }
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
                self.page.error = Some(format!("Could not show this visualization: {error}"));
            }
            window.focus(&self.focus, cx);
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (window, document);
            if let Some(view) = &mut self.fragment {
                view.error = Some(FragmentError::Unsupported);
            }
        }
        cx.notify();
    }

    pub(super) fn reveal_fragment(&self, cx: &mut Context<Self>) {
        if let Some(path) = self.fragment.as_ref().and_then(|v| v.source.local_path()) {
            cx.reveal_path(&path);
        }
    }

    pub(super) fn open_fragment_preview(&self, cx: &mut Context<Self>) {
        if let Some(url) = self
            .fragment
            .as_ref()
            .and_then(|v| v.source.local_preview())
            .and_then(|path| file_url(&path))
        {
            cx.open_url(&url);
        }
    }

    pub(super) fn fragment_toolbar(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(view) = &self.fragment else {
            return div().into_any_element();
        };
        let local = view.source.local_root.is_some();
        let preview = view.source.local_preview().is_some();
        let label = |id: &'static str, text: &'static str, glyph: &'static str, enabled: bool| {
            crate::files::toolbar_button(id, text)
                .when(!enabled, |el| el.cursor_default().opacity(0.35))
                .child(
                    icons::icon(glyph)
                        .size(px(surface_chrome::ICON_SIZE))
                        .text_color(theme.text_muted),
                )
        };
        let title: SharedString = view.source.title.clone().into();
        let path: SharedString = view.source.path.clone().into();
        surface_chrome::toolbar(theme)
            .child(
                label(
                    "fragment-reload",
                    "Reload visualization",
                    icons::REFRESH,
                    true,
                )
                .on_click(cx.listener(|this, _, window, cx| this.reload_fragment(window, cx))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_baseline()
                    .gap(px(6.0))
                    .child(
                        div()
                            .flex_none()
                            .max_w(px(260.0))
                            .truncate()
                            .text_size(crate::typography::ui_rems(12.0))
                            .text_color(theme.text)
                            .child(title),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.text_faint)
                            .child(path),
                    ),
            )
            .child(
                label(
                    "fragment-preview",
                    "Open preview in browser",
                    icons::ARROW_UP_RIGHT,
                    preview,
                )
                .when(preview, |el| {
                    el.on_click(cx.listener(|this, _, _, cx| this.open_fragment_preview(cx)))
                }),
            )
            .child(
                label("fragment-reveal", "Reveal in Finder", icons::FOLDER, local)
                    .when(local, |el| {
                        el.on_click(cx.listener(|this, _, _, cx| this.reveal_fragment(cx)))
                    }),
            )
            .child(
                label("fragment-source", "View source", icons::FILE_CODE, true).on_click(
                    cx.listener(|this, _, _, cx| {
                        if let Some(view) = &this.fragment {
                            cx.emit(BrowserEvent::ViewSource(view.source.path.clone()));
                        }
                    }),
                ),
            )
            .into_any_element()
    }

    /// Loading or error content; `None` once the native page shows the
    /// visualization.
    pub(super) fn fragment_status(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let view = self.fragment.as_ref()?;
        let (title, message, retry): (SharedString, SharedString, bool) =
            if let Some(error) = &view.error {
                (error.title().into(), error.message().into(), true)
            } else if let Some(error) = &self.page.error {
                (
                    "Couldn’t show this visualization".into(),
                    error.clone().into(),
                    true,
                )
            } else if view.loading || view.html.is_none() {
                (
                    "Loading visualization…".into(),
                    view.source.path.clone().into(),
                    false,
                )
            } else {
                return None;
            };
        Some(
            div()
                .id("fragment-status")
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p(px(24.0))
                .child(
                    div()
                        .w_full()
                        .max_w(px(340.0))
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(10.0))
                        .child(
                            div()
                                .text_size(crate::typography::ui_rems(14.0))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme.text)
                                .child(title),
                        )
                        .child(
                            div()
                                .text_center()
                                .text_size(crate::typography::ui_rems(12.0))
                                .line_height(px(19.0))
                                .text_color(theme.text_muted)
                                .child(message),
                        )
                        .when(retry, |el| {
                            el.child(
                                div()
                                    .flex()
                                    .gap(px(8.0))
                                    .child(
                                        status_button("fragment-retry", "Try again", theme)
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.reload_fragment(window, cx)
                                            })),
                                    )
                                    .child(
                                        status_button("fragment-view-source", "View source", theme)
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                if let Some(view) = &this.fragment {
                                                    cx.emit(BrowserEvent::ViewSource(
                                                        view.source.path.clone(),
                                                    ));
                                                }
                                            })),
                                    ),
                            )
                        }),
                )
                .into_any_element(),
        )
    }

    pub(super) fn fragment_notice(&self, theme: &Theme) -> Option<AnyElement> {
        let notice = self.fragment.as_ref()?.notice.clone()?;
        Some(
            div()
                .px(px(12.0))
                .py(px(8.0))
                .border_b_1()
                .border_color(theme.border)
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(theme.text_muted)
                .child(notice)
                .into_any_element(),
        )
    }
}

fn status_button(
    id: &'static str,
    label: &'static str,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .h(px(28.0))
        .px(px(10.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(theme.border)
        .bg(theme.surface_raised)
        .cursor_pointer()
        .role(gpui::Role::Button)
        .aria_label(label)
        .hover(|style| style.bg(crate::theme::wash(0.10)))
        .flex()
        .items_center()
        .text_size(crate::typography::ui_rems(12.0))
        .text_color(theme.text)
        .child(label)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::BrowserContext;

    #[gpui::test]
    fn loaded_fragments_build_a_themed_sandboxed_document(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            crate::composer::init(cx, Default::default());
            cx.set_global(Theme::default());
        });
        let window = cx.add_window(|window, cx| {
            BrowserSurface::new(BrowserContext::default(), false, window, cx)
        });
        window
            .update(cx, |browser, window, cx| {
                let source = FragmentSource {
                    path: "work/a.fragment.html".into(),
                    title: "Kit graph".into(),
                    wide: true,
                    local_root: None,
                };
                browser.open_fragment(source, None, Some("Heads up".into()), window, cx);
                let view = browser.fragment().unwrap();
                assert_eq!(view.notice.as_deref(), Some("Heads up"));
                assert!(matches!(
                    view.error,
                    Some(FragmentError::Unavailable { .. })
                ));
                assert!(browser.fragment_document(cx).is_none());
                assert_eq!(browser.title().as_ref(), "Kit graph");

                let generation = browser.fragment.as_ref().unwrap().generation;
                browser.fragment_loaded(
                    generation,
                    Ok("<div id=\"a\">hi</div>".into()),
                    window,
                    cx,
                );
                let view = browser.fragment().unwrap();
                assert!(view.error.is_none() && !view.loading);
                let document = browser.fragment_document(cx).unwrap();
                assert!(document.contains("<iframe sandbox=\"allow-scripts\""));
                assert!(document.contains("&lt;div id=&quot;a&quot;&gt;hi&lt;/div&gt;"));
                assert!(document.contains("max-width:1024px"));
                let background = FragmentTheme::from_theme(&Theme::default())
                    .vars
                    .into_iter()
                    .find(|(name, _)| *name == "--background")
                    .unwrap()
                    .1;
                assert!(document.contains(&format!("--background: {background};")));

                // A stale read never replaces newer content.
                browser.fragment_loaded(
                    generation - 1,
                    Err(FragmentError::Missing {
                        path: "work/a.fragment.html".into(),
                    }),
                    window,
                    cx,
                );
                assert!(browser.fragment().unwrap().html.is_some());
                // A failed reload is shown as an actionable error.
                browser.fragment_loaded(
                    generation,
                    Err(FragmentError::Missing {
                        path: "work/a.fragment.html".into(),
                    }),
                    window,
                    cx,
                );
                let error = browser.fragment().unwrap().error.clone().unwrap();
                assert_eq!(error.title(), "Visualization file not found");
                assert!(browser.fragment_document(cx).is_none());
            })
            .unwrap();
    }

    #[test]
    fn local_previews_resolve_beside_the_fragment() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("work")).unwrap();
        let mut source = FragmentSource {
            path: "work/a.fragment.html".into(),
            title: "A".into(),
            wide: false,
            local_root: Some(dir.path().into()),
        };
        assert_eq!(source.local_preview(), None);
        std::fs::write(dir.path().join("work/a.preview.html"), "x").unwrap();
        let preview = source.local_preview().unwrap();
        assert!(file_url(&preview).unwrap().starts_with("file:///"));
        source.local_root = None;
        assert_eq!(source.local_preview(), None);
        assert_eq!(source.local_path(), None);
    }
}
