//! A single-choice dropdown menu body: rows with an optional heading or
//! separator before them, a check on the current choice, and a keyboard
//! cursor (↑/↓ wrap, ↩ picks, Esc closes: [`choice_key`]). The caller owns
//! the [`super::Popup`], mounts the body in an anchored card
//! ([`super::anchored_menu_below`]), and maps a picked index back to its
//! own choice. Used by the Inbox column's dropdowns (GitHub scope, PR
//! review destination, PR row overflow, Linear project).
use std::rc::Rc;

use gpui::{Context, SharedString, Window, div, prelude::*, px};

use super::{MenuKey, classify_key, menu_step};
use crate::theme::Theme;

/// One row of a [`choice_menu_body`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChoiceRow {
    /// Element id and debug selector (unique within the window).
    pub id: SharedString,
    pub label: SharedString,
    /// A faint trailing detail (a path, a count).
    pub detail: Option<SharedString>,
    pub checked: bool,
    pub icon: Option<&'static str>,
    /// A small uppercase heading drawn above this row.
    pub heading: Option<SharedString>,
    pub separator_before: bool,
}

impl ChoiceRow {
    pub(crate) fn new(id: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            detail: None,
            checked: false,
            icon: None,
            heading: None,
            separator_before: false,
        }
    }

    pub(crate) fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }

    pub(crate) fn icon(mut self, icon: &'static str) -> Self {
        self.icon = Some(icon);
        self
    }

    pub(crate) fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub(crate) fn heading(mut self, heading: impl Into<SharedString>) -> Self {
        self.heading = Some(heading.into());
        self
    }

    pub(crate) fn separated(mut self) -> Self {
        self.separator_before = true;
        self
    }
}

/// What a key press in an open choice menu does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChoiceKey {
    /// Move the keyboard cursor here.
    Moved(Option<usize>),
    Pick(usize),
    Close,
    /// Not a menu key: let it through (typing into a search box).
    Ignored,
}

/// Classify `keystroke` for a menu of `count` rows with the cursor on
/// `active`.
pub(crate) fn choice_key(
    keystroke: &gpui::Keystroke,
    active: Option<usize>,
    count: usize,
) -> ChoiceKey {
    let modifiers = keystroke.modifiers;
    match classify_key(&keystroke.key, modifiers.platform, modifiers.control) {
        MenuKey::Up => ChoiceKey::Moved(menu_step(active, count, -1)),
        MenuKey::Down => ChoiceKey::Moved(menu_step(active, count, 1)),
        MenuKey::Enter => match active.filter(|&ix| ix < count) {
            Some(ix) => ChoiceKey::Pick(ix),
            None => ChoiceKey::Ignored,
        },
        MenuKey::Escape => ChoiceKey::Close,
        _ => ChoiceKey::Ignored,
    }
}

/// Where the cursor starts: on the current choice, else the first row.
pub(crate) fn initial_active(rows: &[ChoiceRow]) -> Option<usize> {
    rows.iter()
        .position(|row| row.checked)
        .or((!rows.is_empty()).then_some(0))
}

/// The rows of a choice menu. A click picks (`on_pick` with the row index);
/// hovering moves the keyboard cursor (`on_hover`).
pub(crate) fn choice_menu_body<V: 'static>(
    rows: &[ChoiceRow],
    active: Option<usize>,
    theme: &Theme,
    on_pick: Rc<dyn Fn(&mut V, usize, &mut Window, &mut Context<V>)>,
    cx: &mut Context<V>,
) -> gpui::Div {
    let popup = theme.for_popup();
    let mut body = div().flex().flex_col().gap(px(1.0));
    for (ix, row) in rows.iter().enumerate() {
        if row.separator_before && ix > 0 {
            body = body.child(super::menu_separator());
        }
        if let Some(heading) = &row.heading {
            body = body.child(super::menu_heading(&popup, heading.as_ref()));
        }
        let id = row.id.clone();
        let selector = id.to_string();
        let pick = on_pick.clone();
        body = body.child(
            super::menu_row_nav(&popup, false, active == Some(ix), id.clone())
                .id(id)
                .debug_selector(move || selector.clone())
                .gap(px(6.0))
                .py(px(4.0))
                .text_size(crate::typography::ui_rems(12.0))
                .child(
                    div()
                        .flex_none()
                        .w(px(12.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .when(row.checked, |el| {
                            el.child(
                                crate::icons::icon(crate::icons::CHECK)
                                    .size(px(11.0))
                                    .text_color(popup.accent),
                            )
                        }),
                )
                .when_some(row.icon, |el, icon| {
                    el.child(
                        crate::icons::icon(icon)
                            .size(px(12.0))
                            .text_color(popup.text_muted),
                    )
                })
                .child(div().flex_1().min_w_0().truncate().child(row.label.clone()))
                .when_some(row.detail.clone(), |el, detail| {
                    el.child(
                        div()
                            .flex_none()
                            .max_w(px(120.0))
                            .truncate()
                            .text_size(crate::typography::ui_rems(10.5))
                            .text_color(popup.text_faint)
                            .child(detail),
                    )
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    pick(this, ix, window, cx)
                })),
        );
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(key: &str) -> gpui::Keystroke {
        gpui::Keystroke::parse(key).unwrap()
    }

    #[test]
    fn keys_wrap_pick_and_close() {
        assert_eq!(choice_key(&key("down"), None, 3), ChoiceKey::Moved(Some(0)));
        assert_eq!(
            choice_key(&key("up"), Some(0), 3),
            ChoiceKey::Moved(Some(2))
        );
        assert_eq!(choice_key(&key("enter"), Some(1), 3), ChoiceKey::Pick(1));
        assert_eq!(choice_key(&key("enter"), None, 3), ChoiceKey::Ignored);
        assert_eq!(choice_key(&key("escape"), Some(1), 3), ChoiceKey::Close);
        assert_eq!(choice_key(&key("a"), Some(1), 3), ChoiceKey::Ignored);
    }

    #[test]
    fn the_cursor_starts_on_the_checked_row() {
        let rows = [
            ChoiceRow::new("a", "A"),
            ChoiceRow::new("b", "B").checked(true),
        ];
        assert_eq!(initial_active(&rows), Some(1));
        assert_eq!(initial_active(&rows[..1]), Some(0));
        assert_eq!(initial_active(&[]), None);
    }
}
