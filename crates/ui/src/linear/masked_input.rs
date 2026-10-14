//! A single-line secret field for the Linear API key: typed or pasted text
//! renders as bullets, Enter submits, Escape clears. The value never leaves
//! this entity except through [`MaskedKeyInput::take`] (sent once with
//! `LinearSetApiKey`), and its `Debug` never prints it.
use gpui::{
    Context, EventEmitter, FocusHandle, Focusable, KeyDownEvent, SharedString, Window, div,
    prelude::*, px,
};

use crate::theme::Theme;

/// Most characters accepted (Linear keys are ~48).
const MAX_CHARS: usize = 256;
/// Bullets shown at most, so the field never reveals the length past this.
const MAX_BULLETS: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MaskedKeyEvent {
    Submitted,
    Edited,
}

pub(crate) struct MaskedKeyInput {
    value: String,
    placeholder: SharedString,
    focus: FocusHandle,
}

impl std::fmt::Debug for MaskedKeyInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MaskedKeyInput")
            .field("len", &self.value.chars().count())
            .finish()
    }
}

impl EventEmitter<MaskedKeyEvent> for MaskedKeyInput {}

impl Focusable for MaskedKeyInput {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl MaskedKeyInput {
    pub(crate) fn new(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        Self {
            value: String::new(),
            placeholder: placeholder.into(),
            focus: cx.focus_handle(),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.value.trim().is_empty()
    }

    /// The trimmed value; clears the field.
    pub(crate) fn take(&mut self, cx: &mut Context<Self>) -> String {
        let value = std::mem::take(&mut self.value).trim().to_string();
        cx.notify();
        value
    }

    pub(crate) fn clear(&mut self, cx: &mut Context<Self>) {
        self.value.clear();
        cx.notify();
    }

    /// Append `text` (paste or typing), dropping whitespace and newlines.
    pub(crate) fn insert(&mut self, text: &str, cx: &mut Context<Self>) {
        let room = MAX_CHARS.saturating_sub(self.value.chars().count());
        self.value.extend(
            text.chars()
                .filter(|c| !c.is_whitespace() && !c.is_control())
                .take(room),
        );
        cx.emit(MaskedKeyEvent::Edited);
        cx.notify();
    }

    /// Paste the clipboard's text.
    pub(crate) fn paste(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.insert(&text, cx);
        }
    }

    /// What the field shows: one bullet per character, capped.
    pub(crate) fn masked(&self) -> String {
        "\u{2022}".repeat(self.value.chars().count().min(MAX_BULLETS))
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let mods = &keystroke.modifiers;
        let primary = if cfg!(target_os = "macos") {
            mods.platform
        } else {
            mods.control
        };
        match keystroke.key.as_str() {
            "enter" => cx.emit(MaskedKeyEvent::Submitted),
            "escape" => {
                if self.value.is_empty() {
                    return;
                }
                self.clear(cx);
            }
            "backspace" => {
                if primary || mods.alt {
                    self.value.clear();
                } else {
                    self.value.pop();
                }
                cx.emit(MaskedKeyEvent::Edited);
                cx.notify();
            }
            "v" if primary => self.paste(cx),
            "a" | "c" | "x" if primary => {}
            _ => {
                if primary || mods.control || mods.function {
                    return;
                }
                match keystroke.key_char.as_deref() {
                    Some(text) if !text.is_empty() => self.insert(text, cx),
                    _ => return,
                }
            }
        }
        window.prevent_default();
        cx.stop_propagation();
    }
}

impl Render for MaskedKeyInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let focused = self.focus.is_focused(window);
        let empty = self.value.is_empty();
        div()
            .id("linear-key-input")
            .debug_selector(|| "linear-key-input".into())
            .key_context("LinearKeyInput")
            .track_focus(&self.focus)
            .flex_1()
            .min_w_0()
            .h(px(28.0))
            .px(px(8.0))
            .flex()
            .items_center()
            .rounded(px(6.0))
            .border_1()
            .border_color(if focused { theme.accent } else { theme.border })
            .bg(theme.surface)
            .cursor_text()
            .text_size(crate::typography::ui_rems(12.5))
            .on_click(cx.listener(|this, _, window, cx| {
                window.focus(&this.focus, cx);
            }))
            .on_key_down(cx.listener(Self::key_down))
            .child(if empty {
                div()
                    .text_color(theme.text_faint)
                    .child(self.placeholder.clone())
            } else {
                div()
                    .text_color(theme.text)
                    .truncate()
                    .child(SharedString::from(self.masked()))
            })
            .when(focused, |el| {
                el.child(div().ml(px(1.0)).w(px(1.0)).h(px(14.0)).bg(theme.accent))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn masks_input_and_drops_whitespace(cx: &mut gpui::TestAppContext) {
        let input = cx.new(|cx| MaskedKeyInput::new("Paste key", cx));
        input.update(cx, |input, cx| input.insert(" lin_api_123\n ", cx));
        input.read_with(cx, |input, _| {
            assert_eq!(input.masked(), "\u{2022}".repeat(11));
            assert!(!format!("{input:?}").contains("lin_api"));
        });
        let value = input.update(cx, |input, cx| input.take(cx));
        assert_eq!(value, "lin_api_123");
        input.read_with(cx, |input, _| assert!(input.is_empty()));
    }
}
