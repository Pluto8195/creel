//! The Linear panel's project dropdown (header chip, shown when more than
//! one project has Linear on): every project, the current one checked;
//! ↑/↓, ↩, Esc. A real menu: the chip never cycles.
use std::rc::Rc;

use gpui::{AnyElement, Context, SharedString, Window, div, prelude::*, px};

use super::LinearPanel;
use crate::popover::choice_menu::{self, ChoiceKey, ChoiceRow};
use crate::theme::Theme;
use crate::work_outline::chips::TextTooltip;

impl LinearPanel {
    fn space_rows(&self) -> Vec<ChoiceRow> {
        self.spaces
            .iter()
            .map(|space| {
                ChoiceRow::new(
                    format!(
                        "linear-space-{}",
                        space.id.to_lowercase().replace(['/', ' '], "-")
                    ),
                    space.label.clone(),
                )
                .checked(space.id == self.space_id)
            })
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn space_menu_open(&self) -> bool {
        self.space_menu.is_open()
    }

    fn close_space_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if crate::motion::reduced_motion(cx) {
            // No exit animation: unmount now.
            self.space_menu = crate::popover::Popup::default();
        } else if self.space_menu.begin_close() {
            crate::popover::reap_popup(cx, |this: &mut Self| &mut this.space_menu);
        }
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn pick_space(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(space_id) = self.spaces.get(ix).map(|space| space.id.clone()) else {
            return;
        };
        self.close_space_menu(window, cx);
        self.set_space(space_id, cx);
    }

    /// ↑/↓/↩/Esc while the dropdown is open.
    pub(super) fn space_menu_key_down(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.space_menu.is_open() {
            return;
        }
        match choice_menu::choice_key(&event.keystroke, self.space_menu_active, self.spaces.len()) {
            ChoiceKey::Moved(active) => {
                cx.stop_propagation();
                self.space_menu_active = active;
                cx.notify();
            }
            ChoiceKey::Pick(ix) => {
                cx.stop_propagation();
                self.pick_space(ix, window, cx);
            }
            ChoiceKey::Close => {
                cx.stop_propagation();
                self.close_space_menu(window, cx);
            }
            ChoiceKey::Ignored => {}
        }
    }

    /// The header's project chip: a dropdown with more than one project, a
    /// plain label otherwise.
    pub(super) fn render_space_chip(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let label = self
            .spaces
            .iter()
            .find(|space| space.id == self.space_id)
            .map(|space| space.label.clone())
            .unwrap_or_default();
        let multi = self.spaces.len() > 1;
        let chip = div()
            .id("linear-panel-space")
            .debug_selector(|| "linear-panel-space".into())
            .min_w_0()
            .truncate()
            .text_size(crate::typography::ui_rems(11.0))
            .text_color(theme.text_muted);
        let chip = if multi {
            chip.px(px(4.0))
                .rounded(px(4.0))
                .cursor_pointer()
                .hover(|el| el.text_color(theme.text).bg(theme.element_hover))
                .child(SharedString::from(format!("{label} \u{25be}")))
                .tooltip(|_, cx| cx.new(|_| TextTooltip("Choose the project".into())).into())
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _, _, _| this.space_menu.note_trigger_press()),
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    if this.space_menu.take_press_was_open() {
                        this.close_space_menu(window, cx);
                        return;
                    }
                    this.space_menu.open(());
                    this.space_menu_active = choice_menu::initial_active(&this.space_rows());
                    window.focus(&this.focus, cx);
                    cx.notify();
                }))
        } else {
            chip.child(SharedString::from(label))
        };
        let menu = self.space_menu.get().is_some().then(|| {
            let popup = theme.for_popup();
            let pick: Rc<dyn Fn(&mut Self, usize, &mut Window, &mut Context<Self>)> =
                Rc::new(|this, ix, window, cx| this.pick_space(ix, window, cx));
            let body = choice_menu::choice_menu_body(
                &self.space_rows(),
                self.space_menu_active,
                theme,
                pick,
                cx,
            );
            let card = crate::popover::popover_card(&popup)
                .debug_selector(|| "linear-space-menu".into())
                .w(px(220.0))
                .on_mouse_down_out(
                    cx.listener(|this, _, window, cx| this.close_space_menu(window, cx)),
                )
                .child(
                    div()
                        .id("linear-space-menu-list")
                        .max_h(px(280.0))
                        .overflow_y_scroll()
                        .child(body),
                );
            crate::popover::anchored_menu_below(
                "linear-space-menu",
                card.into_any_element(),
                self.space_menu.closing_since(),
            )
        });
        div()
            .relative()
            .flex_1()
            .min_w_0()
            .flex()
            .child(chip)
            .children(menu)
            .into_any_element()
    }
}
