//! The chat header (breadcrumb, title, chips, attention badges), its hover
//! context card, and the side panel's sticky "Item" tab. All three read one
//! [`ChatItemFacts`] sheet, so they cannot disagree.
//!
//! Hover and pin state is Shell-local and only notifies when the card's
//! visibility actually changes; nothing here takes focus (the composer keeps
//! the keyboard), and the facts come from the same state slices the old header
//! already read, so the Shell's observer gating stays as it was.
use super::*;

use crate::work_outline::hints::{HintKey, WithHint};
use crate::chat_item::{
    CardTimer, ChatItemFacts, FactsInput, ItemSection, PathSegment, facts,
};

/// Height of the breadcrumb/title row the new header adds above the chips; the
/// transcript's top fade clears it.
pub(super) const CHAT_HEADER_TITLE_ROW_CLEARANCE: f32 = 28.0;
/// The same with the breadcrumb stacked above the title.
pub(super) const CHAT_HEADER_STACKED_CLEARANCE: f32 = 44.0;
/// Chips shown for the chat; the card and the tab carry the rest.
const CARD_WIDTH: f32 = 520.0;
/// Rows of children listed before "+N more".
const CHILD_ROWS: usize = 8;

/// Chip colouring: plain, dimmed (branch), amber (state drift) or link (blue
/// border and icon, the PR and ticket chips).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ChipTone {
    Plain,
    Dim,
    Warn,
    Link,
}

/// Border and text colours per chip tone, all theme tokens.
pub(super) fn chip_palette(theme: &Theme, tone: ChipTone) -> (gpui::Hsla, gpui::Hsla) {
    match tone {
        ChipTone::Plain => (theme.border_strong, theme.text),
        ChipTone::Dim => (theme.border, theme.text_muted),
        ChipTone::Warn => (theme.warning, theme.warning),
        ChipTone::Link => (theme.accent, theme.text),
    }
}

/// The icon that leads each section label.
fn section_icon(name: &str) -> &'static str {
    match name {
        "path" => crate::icons::FOLDER_WITH_FILES,
        "instructions" => crate::icons::DOCUMENT,
        "runs-in" => crate::icons::TERMINAL,
        "model" => crate::icons::BOT,
        "account" => crate::icons::KEY_MINIMALISTIC,
        "links" => crate::icons::PULL_REQUEST,
        "needs-you" => crate::icons::BELL,
        "plan" => crate::icons::CHECKLIST,
        "children" => crate::icons::LIST,
        "work-item" => crate::icons::PLUS,
        _ => crate::icons::TAG,
    }
}

/// A section label: icon, then the name in small muted semibold caps.
fn section_label(
    theme: &Theme,
    icon_path: &'static str,
    label: &str,
    icon_selector: String,
) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(5.0))
        .child(
            div()
                .debug_selector(move || icon_selector.clone())
                .flex_none()
                .child(
                    crate::icons::icon(icon_path)
                        .size(px(11.0))
                        .text_color(theme.text_muted),
                ),
        )
        .child(
            div()
                .text_size(crate::typography::ui_rems(10.5))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme.text_muted)
                .child(SharedString::from(label.to_uppercase())),
        )
}

/// Where a section list renders: the tab (full) or the hover card (condensed).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Surface {
    Tab,
    Card,
}

impl Surface {
    fn prefix(self) -> &'static str {
        match self {
            Self::Tab => "chat-item",
            Self::Card => "chat-item-card",
        }
    }
}

impl Shell {
    /// The selected chat's fact sheet (never fetches; the header-link cache is
    /// filled by `ensure_chat_header_links`).
    pub(super) fn chat_item_facts(&self, chat_id: &str, cx: &App) -> Option<ChatItemFacts> {
        let state = self.state.read(cx);
        let chat = state.chats.iter().find(|chat| chat.id == chat_id)?;
        let space = chat.space_id.as_deref().and_then(|id| state.space_row(id));
        let snapshot = chat
            .space_id
            .as_deref()
            .and_then(|id| state.work_outline(id).snapshot().cloned());
        let artifacts = chat
            .space_id
            .as_deref()
            .and_then(|id| state.artifacts().snapshot(id).cloned());
        let links = self
            .chat_header_links
            .get(chat_id)
            .and_then(|entry| entry.status.as_ref())
            .map(crate::chat_metadata::HeaderLinks::from_status);
        Some(facts(&FactsInput {
            chat,
            space_path: space.map(|space| space.path.as_str()),
            account: state.chat_account_badge(chat_id),
            links: links.as_ref(),
            snapshot: snapshot.as_deref(),
            artifacts: artifacts.as_deref(),
        }))
    }

    // ----------------------------------------------------------------------
    // Hover card state
    // ----------------------------------------------------------------------

    fn run_card_timer(&mut self, timer: Option<CardTimer>, cx: &mut Context<Self>) {
        let Some(timer) = timer else {
            return;
        };
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(timer.delay_ms()))
                .await;
            this.update(cx, |this, cx| {
                if this.chat_item_card.fire(timer) {
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn chat_item_header_hover(&mut self, on: bool, cx: &mut Context<Self>) {
        let timer = self.chat_item_card.header_hover(on);
        self.run_card_timer(timer, cx);
    }

    fn chat_item_card_hover(&mut self, on: bool, cx: &mut Context<Self>) {
        let timer = self.chat_item_card.card_hover(on);
        self.run_card_timer(timer, cx);
    }

    /// ⌥⌘I: pin the context card open, or release it.
    pub(crate) fn toggle_chat_item_card_pin(&mut self, cx: &mut Context<Self>) {
        if self.state.read(cx).selected_chat.is_none() {
            return;
        }
        self.chat_item_card.toggle_pin();
        cx.notify();
    }

    /// Open the side panel on its Item tab (opening the panel if needed).
    pub(crate) fn open_item_tab(&mut self, cx: &mut Context<Self>) {
        if self.active_chat.is_empty() {
            return;
        }
        self.leave_overview_for_chat(cx);
        self.set_surfaces_open(true, cx);
        self.set_right_active(RightSurface::Item, cx);
        self.chat_item_card.dismiss();
        cx.notify();
    }

    // ----------------------------------------------------------------------
    // Header
    // ----------------------------------------------------------------------

    /// Metadata floating immediately under the overlaid titlebar: a trimmed
    /// clickable breadcrumb, the title, the attention badges, and one chip row
    /// (model, account, branch, PR, ticket). Participates in the column's flex
    /// sizing so wrapping chips never collide with the composer below.
    pub(super) fn render_chat_header_metadata(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let chat_id = self.state.read(cx).selected_chat.clone()?;
        self.ensure_chat_header_links(chat_id.clone(), cx);
        if self.chat_item_card_chat != chat_id {
            self.chat_item_card_chat = chat_id.clone();
            self.chat_item_card.dismiss();
        }
        let facts = self.chat_item_facts(&chat_id, cx)?;
        let card = self
            .chat_item_card
            .visible()
            .then(|| self.render_item_card(&facts, theme, cx));
        let crumbs = self.render_header_breadcrumb(&facts, theme, cx);
        let badges = self.render_header_badges(&facts, theme, cx);
        let chips = self.render_header_chips(&facts, theme, cx);
        let title = div()
            .debug_selector(|| "chat-header-title".into())
            .min_w_0()
            .truncate()
            .text_size(crate::typography::ui_rems(15.0))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(theme.text)
            .child(SharedString::from(facts.title.clone()));
        let stacked = crumbs.is_some();
        self.chat_header_extra_clearance = if stacked {
            CHAT_HEADER_STACKED_CLEARANCE
        } else {
            CHAT_HEADER_TITLE_ROW_CLEARANCE
        };
        Some(
            div()
                .id("full-chat-header-metadata-wrap")
                .debug_selector(|| "chat-header".into())
                .relative()
                .flex_none()
                .mt(px(Theme::TITLEBAR_HEIGHT))
                .px(px(12.0))
                .py(px(4.0))
                .border_b_1()
                .border_color(theme.border.opacity(0.7))
                .bg(theme.surface.opacity(0.94))
                .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                    this.chat_item_header_hover(*hovered, cx)
                }))
                // Looking at the header must never move the keyboard off the
                // composer.
                .on_mouse_down(gpui::MouseButton::Left, |_, window, _| {
                    window.prevent_default()
                })
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .min_w_0()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .when_some(crumbs, |col, crumbs| col.child(crumbs))
                                .child(title),
                        )
                        .children(badges),
                )
                .child(chips)
                .children(card)
                .into_any_element(),
        )
    }

    fn render_header_breadcrumb(
        &mut self,
        facts: &ChatItemFacts,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let item = facts.item.as_ref()?;
        let (trimmed, segments) = item.trimmed_path(2);
        let full: SharedString = item
            .path
            .iter()
            .map(|segment| segment.title.as_str())
            .collect::<Vec<_>>()
            .join(" \u{203a} ")
            .into();
        let mut row = div()
            .id("chat-header-breadcrumb")
            .debug_selector(|| "chat-header-breadcrumb".into())
            .flex_none()
            .max_w(px(280.0))
            .flex()
            .items_center()
            .gap(px(4.0))
            .text_size(crate::typography::ui_rems(12.0))
            .text_color(theme.text_muted)
            .tooltip(move |_, cx| {
                cx.new(|_| SurfaceTabTooltip { text: full.clone() }).into()
            });
        if trimmed {
            row = row.child(div().text_color(theme.text_faint).child("\u{2026} \u{203a}"));
        }
        let last = segments.len().saturating_sub(1);
        for (ix, segment) in segments.iter().enumerate() {
            let space_id = item.space_id.clone();
            let item_id = segment.id.clone();
            if ix > 0 {
                row = row.child(div().text_color(theme.text_faint).child("\u{203a}"));
            }
            row = row.child(
                div()
                    .id(("chat-header-crumb", ix))
                    .debug_selector(move || format!("chat-header-crumb-{ix}"))
                    .min_w_0()
                    .truncate()
                    .rounded(px(3.0))
                    .px(px(2.0))
                    .cursor_pointer()
                    .when(ix == last, |el| {
                        el.font_weight(gpui::FontWeight::MEDIUM).text_color(theme.text)
                    })
                    .hover(|el| el.bg(theme.element_hover).text_color(theme.text))
                    .child(SharedString::from(segment.title.clone()))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.reveal_work_item_in_outline(&space_id, &item_id, window, cx)
                    })),
            );
        }
        Some(row.into_any_element())
    }

    fn render_header_badges(
        &mut self,
        facts: &ChatItemFacts,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(item) = facts.item.as_ref() else {
            return Vec::new();
        };
        let badge = |id: &'static str, glyph: &'static str, count: usize, color: gpui::Hsla| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .flex_none()
                .h(px(18.0))
                .px(px(5.0))
                .flex()
                .items_center()
                .gap(px(3.0))
                .rounded(px(5.0))
                .border_1()
                .border_color(color)
                .cursor_pointer()
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(color)
                .hover(|el| el.bg(theme.element_hover))
                .child(glyph)
                .when(count > 0, |el| el.child(SharedString::from(count.to_string())))
                .on_click(cx.listener(|this, _, _, cx| this.open_item_tab(cx)))
        };
        let mut out = Vec::new();
        let attention = item.attention_count();
        if attention > 0 {
            out.push(
                badge("chat-header-badge-attention", "\u{26a0}", attention, theme.warning)
                    .hint(
                        "Open actions or changed instructions \u{2014} open the Item tab",
                        HintKey::None,
                    )
                    .into_any_element(),
            );
        }
        let questions = item.question_count();
        if questions > 0 {
            out.push(
                badge("chat-header-badge-questions", "\u{25cf}", questions, theme.accent)
                    .hint(
                        "Unanswered plan questions \u{2014} open the Item tab",
                        HintKey::None,
                    )
                    .into_any_element(),
            );
        }
        out
    }

    fn render_header_chips(
        &mut self,
        facts: &ChatItemFacts,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let chip = |key: String,
                    icon_path: &'static str,
                    tone: ChipTone,
                    label: String,
                    tooltip: String,
                    url: Option<String>| {
            let tooltip: SharedString = tooltip.into();
            let id: SharedString = format!("chat-header-chip-{key}").into();
            let selector = id.to_string();
            let icon_selector = format!("{selector}-icon");
            let clickable = url.is_some();
            let (border, text) = chip_palette(theme, tone);
            div()
                .id(id)
                .debug_selector(move || selector.clone())
                .max_w(px(230.0))
                .h(px(20.0))
                .px(px(7.0))
                .flex()
                .items_center()
                .gap(px(4.0))
                .rounded(px(5.0))
                .border_1()
                .border_color(border)
                .bg(theme.element_hover.opacity(0.55))
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(text)
                .when(clickable, |chip| {
                    chip.cursor_pointer().hover(|chip| chip.bg(theme.element_hover))
                })
                .child(
                    div()
                        .debug_selector(move || icon_selector.clone())
                        .flex_none()
                        .child(
                            crate::icons::icon(icon_path)
                                .size(px(11.0))
                                .text_color(if tone == ChipTone::Link { theme.accent } else { text }),
                        ),
                )
                .child(div().min_w_0().truncate().child(SharedString::from(label)))
                .tooltip(move |_, cx| {
                    cx.new(|_| SurfaceTabTooltip {
                        text: tooltip.clone(),
                    })
                    .into()
                })
                .when_some(url, |chip, url| {
                    chip.on_click(cx.listener(move |this, _, window, cx| {
                        let external = window.modifiers().secondary();
                        this.open_web_reference(&url, external, window, cx)
                    }))
                })
        };
        let mut row = div()
            .debug_selector(|| "chat-header-chips".into())
            .w_full()
            .min_w_0()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(4.0));
        // The model chip turns amber when the run used another model than
        // the one configured.
        let model_drift = facts.model.ran && facts.model.configured.is_some();
        row = row.child(chip(
            "model".into(),
            crate::icons::BOT,
            if model_drift { ChipTone::Warn } else { ChipTone::Plain },
            facts.model.chip(),
            facts.model.tooltip.clone(),
            None,
        ));
        if let Some(account) = &facts.account {
            row = row.child(chip(
                "account".into(),
                crate::icons::KEY_MINIMALISTIC,
                ChipTone::Plain,
                account.clone(),
                format!("Agent account: {account}"),
                None,
            ));
        }
        if let Some(branch) = &facts.branch {
            row = row.child(chip(
                "branch".into(),
                crate::icons::GIT_BRANCH,
                ChipTone::Dim,
                branch.clone(),
                format!("Branch: {branch}"),
                None,
            ));
        }
        for (ix, pr) in facts.prs.iter().enumerate() {
            let label = if pr.number > 0 {
                format!("PR #{}", pr.number)
            } else {
                "Pull request".into()
            };
            let tooltip = pr.title.clone().unwrap_or_else(|| pr.url.clone());
            let url = (!pr.url.is_empty()).then(|| pr.url.clone());
            row = row.child(chip(
                format!("pr-{ix}"),
                crate::icons::PULL_REQUEST,
                ChipTone::Link,
                label,
                tooltip,
                url,
            ));
        }
        if let Some(ticket) = &facts.ticket {
            row = row.child(chip(
                "ticket".into(),
                crate::icons::TAG,
                ChipTone::Link,
                ticket.identifier.clone(),
                ticket
                    .title
                    .clone()
                    .unwrap_or_else(|| ticket.identifier.clone()),
                ticket.url.clone(),
            ));
        }
        row.into_any_element()
    }

    // ----------------------------------------------------------------------
    // Hover card
    // ----------------------------------------------------------------------

    fn render_item_card(
        &mut self,
        facts: &ChatItemFacts,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let pinned = self.chat_item_card.pinned();
        let body = self.render_item_sections(facts, Surface::Card, theme, cx);
        div()
            .id("chat-item-card")
            .debug_selector(|| "chat-item-card".into())
            .absolute()
            .top_full()
            .left(px(12.0))
            .w(px(CARD_WIDTH))
            .max_w_full()
            .mt(px(4.0))
            .p(px(10.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(theme.border_strong)
            .bg(theme.surface_raised)
            .shadow_md()
            .occlude()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                this.chat_item_card_hover(*hovered, cx)
            }))
            .on_mouse_down(gpui::MouseButton::Left, |_, window, _| {
                window.prevent_default()
            })
            .child(body)
            .child(
                div()
                    .debug_selector(|| "chat-item-card-footer".into())
                    .text_size(crate::typography::ui_rems(10.0))
                    .text_color(theme.text_faint)
                    .child(if pinned {
                        "\u{2325}\u{2318}I unpins \u{b7} pinned open"
                    } else {
                        "\u{2325}\u{2318}I pins this card open"
                    }),
            )
            .into_any_element()
    }

    // ----------------------------------------------------------------------
    // Item tab
    // ----------------------------------------------------------------------

    /// The sticky first chip of the right pane's tab strip.
    pub(super) fn render_item_tab_chip(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let active = self.resolved_right_active(cx) == RightSurface::Item;
        let needs_you = self
            .chat_item_facts(&self.active_chat.clone(), cx)
            .and_then(|facts| facts.item)
            .is_some_and(|item| item.attention_count() + item.question_count() > 0);
        div()
            .id("right-surface-item-tab")
            .debug_selector(|| "right-surface-item-tab".into())
            .h(px(24.0))
            .flex_none()
            .px(px(8.0))
            .rounded(px(6.0))
            .flex()
            .items_center()
            .gap(px(5.0))
            .cursor_pointer()
            .role(gpui::Role::Button)
            .aria_label("Item")
            .block_mouse_except_scroll()
            .on_mouse_down(gpui::MouseButton::Left, |_, window, _| {
                window.prevent_default()
            })
            .when(active, |el| el.bg(crate::theme::wash(0.10)))
            .when(!active, |el| el.hover(|s| s.bg(crate::theme::wash(0.06))))
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                this.set_right_active(RightSurface::Item, cx);
            }))
            .child(
                div()
                    .text_size(crate::typography::ui_rems(11.5))
                    .text_color(if active { theme.text } else { theme.text_muted })
                    .child("Item"),
            )
            .when(needs_you, |el| {
                el.child(
                    div()
                        .debug_selector(|| "right-surface-item-dot".into())
                        .flex_none()
                        .size(px(6.0))
                        .rounded_full()
                        .bg(theme.warning),
                )
            })
            .into_any_element()
    }

    /// The Item tab's body.
    pub(super) fn render_item_tab(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let chat_id = self.active_chat.clone();
        let Some(facts) = self.chat_item_facts(&chat_id, cx) else {
            return div().size_full().into_any_element();
        };
        let body = self.render_item_sections(&facts, Surface::Tab, &theme, cx);
        div()
            .id("chat-item-tab")
            .debug_selector(|| "chat-item-tab".into())
            .size_full()
            .overflow_y_scroll()
            .p(px(14.0))
            .child(body)
            .into_any_element()
    }

    // ----------------------------------------------------------------------
    // Shared sections
    // ----------------------------------------------------------------------

    fn render_item_sections(
        &mut self,
        facts: &ChatItemFacts,
        surface: Surface,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let prefix = surface.prefix();
        let tab = surface == Surface::Tab;
        let section = |name: &'static str, label: &'static str| {
            let selector = format!("{prefix}-{name}");
            let icon_selector = format!("{prefix}-{name}-icon");
            let icon_path = section_icon(name);
            div()
                .debug_selector(move || selector.clone())
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(section_label(theme, icon_path, label, icon_selector))
        };
        let text = |value: String| {
            div()
                .min_w_0()
                .text_size(crate::typography::ui_rems(12.0))
                .text_color(theme.text)
                .child(SharedString::from(value))
        };
        let muted = |value: String| {
            div()
                .min_w_0()
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(theme.text_muted)
                .child(SharedString::from(value))
        };
        // Content links read as primary text with an underline; actions
        // (Edit, Answer, Add to outline) take the success tone.
        let link_with = |id: String, label: &'static str, color: gpui::Hsla, underline: bool| {
            let selector = id.clone();
            div()
                .id(SharedString::from(id))
                .debug_selector(move || selector.clone())
                .flex_none()
                .px(px(4.0))
                .rounded(px(3.0))
                .cursor_pointer()
                .text_size(crate::typography::ui_rems(11.5))
                .text_color(color)
                .when(underline, |el| el.underline())
                .hover(|el| el.bg(theme.element_hover))
                .when(!label.is_empty(), |el| el.child(label))
        };
        let link = |id: String, label: &'static str| link_with(id, label, theme.text, true);
        let action_link = |id: String, label: &'static str| link_with(id, label, theme.success, false);
        let mut col = div().flex().flex_col().gap(px(12.0));

        // Path (the tab prints it in full; the card's header already shows it).
        if tab && let Some(item) = &facts.item {
            col = col.child(section("path", "Path").child(self.render_item_path(item, theme, cx)));
        }

        // Instructions.
        if let Some(item) = &facts.item {
            let space_id = item.space_id.clone();
            let item_id = item.item_id.clone();
            let summary = if item.instructions.inherited > 0 {
                format!(
                    "{} own \u{b7} +{} inherited",
                    item.instructions.own, item.instructions.inherited
                )
            } else {
                format!("{} own", item.instructions.own)
            };
            let status = self.render_chat_instruction_status(&facts.chat_id, theme, cx);
            let proposals = self.render_chat_proposal_status(&facts.chat_id, theme, cx);
            col = col.child(
                section("instructions", "Instructions")
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .child(if item.instructions.drift {
                                muted(format!("{summary} \u{b7} changed")).text_color(theme.warning)
                            } else {
                                muted(format!("{summary} \u{b7} up to date"))
                            })
                            .child(action_link(format!("{prefix}-instructions-edit"), "Edit").on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.open_work_item_in_overview(
                                        space_id.clone(),
                                        item_id.clone(),
                                        cx,
                                    )
                                }),
                            )),
                    )
                    .children(status)
                    .children(proposals),
            );
        }

        // Runs in, model and account: the card leaves them to the header chips.
        if tab {
        let runs_in = {
            let mut block = section("runs-in", "Runs in");
            if let Some(path) = &facts.runs_in {
                block = block.child(text(path.clone()));
            }
            if let Some(branch) = &facts.branch {
                block = block.child(muted(format!("\u{2387} {branch}")));
            }
            if facts.runs_in.is_none() && facts.branch.is_none() {
                block = block.child(muted("Not started yet".into()));
            }
            block
        };
        col = col.child(runs_in);

        // Model and account.
        let model_differs = facts.model.ran && facts.model.configured.is_some();
        col = col.child(
            section("model", "Model").child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_baseline()
                    .gap(px(6.0))
                    .child(text(
                        facts.model.label.clone().unwrap_or_else(|| "default".into()),
                    ))
                    .child(
                        muted(format!("\u{b7} {}", facts.model.detail()))
                            .when(model_differs, |el| el.text_color(theme.warning)),
                    ),
            ),
        );
        col = col.child(section("account", "Account").child(match &facts.account {
            Some(account) => div()
                .debug_selector(|| "chat-item-account-badge".into())
                .flex_none()
                .h(px(20.0))
                .px(px(7.0))
                .flex()
                .items_center()
                .gap(px(4.0))
                .rounded(px(5.0))
                .border_1()
                .border_color(theme.border_strong)
                .bg(theme.element_hover.opacity(0.55))
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(theme.text)
                .child(
                    crate::icons::icon(crate::icons::KEY_MINIMALISTIC)
                        .size(px(11.0))
                        .text_color(theme.text),
                )
                .child(SharedString::from(account.clone())),
            None => muted("Default login".into()),
        }));
        }

        // Links.
        if let Some(item) = &facts.item
            && (!facts.prs.is_empty() || facts.ticket.is_some())
        {
            let _ = item;
            let mut block = section("links", "Links");
            for (ix, pr) in facts.prs.iter().enumerate() {
                let url = pr.url.clone();
                let label = if pr.number > 0 {
                    format!("PR #{}", pr.number)
                } else {
                    "Pull request".into()
                };
                block = block.child(
                    link(format!("{prefix}-pr-{ix}"), "").child(SharedString::from(label)).on_click(
                        cx.listener(move |this, _, window, cx| {
                            let external = window.modifiers().secondary();
                            this.open_web_reference(&url, external, window, cx)
                        }),
                    ),
                );
            }
            if let Some(ticket) = &facts.ticket {
                let url = ticket.url.clone();
                block = block.child(
                    link(format!("{prefix}-ticket"), "")
                        .child(SharedString::from(format!("Ticket {}", ticket.identifier)))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if let Some(url) = &url {
                                let external = window.modifiers().secondary();
                                this.open_web_reference(url, external, window, cx)
                            }
                        })),
                );
            }
            col = col.child(block);
        }

        // Needs you.
        if let Some(item) = &facts.item
            && !item.actions.is_empty()
        {
            let mut block = section("needs-you", "Needs you");
            for (ix, action) in item.actions.iter().enumerate() {
                let space_id = item.space_id.clone();
                let item_id = item.item_id.clone();
                block = block.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .child(
                            div().flex_none().text_color(if action.plan_question {
                                theme.accent
                            } else {
                                theme.warning
                            }).child(if action.plan_question { "\u{25cf}" } else { "\u{26a0}" }),
                        )
                        .child(div().flex_1().min_w_0().truncate().child(text(action.text.clone())))
                        .child(action_link(format!("{prefix}-answer-{ix}"), "Answer").on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.open_work_item_in_overview(
                                    space_id.clone(),
                                    item_id.clone(),
                                    cx,
                                )
                            }),
                        )),
                );
            }
            col = col.child(block);
        }

        // Plan.
        if let Some(plan) = facts.item.as_ref().and_then(|item| item.plan.as_ref()) {
            let id = plan.artifact_id.clone();
            let changed = crate::artifacts::questions_ui::list_marker(
                &plan.artifact_id,
                plan.revision,
                &crate::settings::current(cx).artifact_last_viewed,
            );
            col = col.child(
                section("plan", "Plan").child(
                    link(format!("{prefix}-plan-open"), "")
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .child(SharedString::from(format!(
                            "{} \u{b7} rev {}",
                            plan.title, plan.revision
                        )))
                        .children(changed.map(|(from, to)| {
                            crate::artifacts::questions_ui::changed_dot(
                                format!("{prefix}-plan-changed"),
                                from,
                                to,
                                theme,
                            )
                        }))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_artifact(id.clone(), window, cx)
                        })),
                ),
            );
        }

        // Children (the tab only).
        if tab
            && let Some(item) = &facts.item
            && !item.children.is_empty()
        {
            let mut block = section("children", "Children");
            for child in item.children.iter().take(CHILD_ROWS) {
                let space_id = item.space_id.clone();
                let child_id = child.id.clone();
                let glyph = if child.done {
                    crate::icons::icon(crate::icons::CHECK)
                        .size(px(10.0))
                        .text_color(theme.success)
                        .into_any_element()
                } else {
                    div()
                        .flex_none()
                        .size(px(8.0))
                        .rounded_full()
                        .border_1()
                        .border_color(theme.text_muted)
                        .into_any_element()
                };
                block = block.child(
                    link(format!("{prefix}-child-{}", child.id), "")
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .child(div().w(px(10.0)).flex().justify_center().child(glyph))
                        .child(SharedString::from(
                            if child.title.trim().is_empty() {
                                "Untitled"
                            } else {
                                child.title.as_str()
                            }
                            .to_owned(),
                        ))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_work_item_in_overview(space_id.clone(), child_id.clone(), cx)
                        })),
                );
            }
            if item.children.len() > CHILD_ROWS {
                block = block.child(muted(format!("+{} more", item.children.len() - CHILD_ROWS)));
            }
            col = col.child(block);
        }

        // No item: the way in.
        if facts.item.is_none() {
            let chat_id = facts.chat_id.clone();
            col = col.child(
                section("work-item", "Work item")
                    .child(muted("No item. This chat is only in its project.".into()))
                    .child(
                        action_link(format!("{prefix}-add-to-outline"), "Add to outline").on_click(
                            cx.listener(move |this, _, window, cx| {
                                this.reveal_chat_in_outline(&chat_id, window, cx)
                            }),
                        ),
                    ),
            );
        }
        // Identifiers (the tab only): chat id, harness session id, workspace.
        if tab {
            col = col.child(self.render_identifiers(facts, theme, cx));
        }
        col.into_any_element()
    }

    /// Copy one identifier; its button flips to "Copied" for ~1.2s.
    pub(crate) fn copy_identifier(&mut self, key: &str, value: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(value));
        self.copied_chat_identity = Some(key.to_string());
        self.copied_chat_identity_clear = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1200))
                .await;
            this.update(cx, |this, cx| {
                this.copied_chat_identity = None;
                this.copied_chat_identity_clear = None;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn render_identifiers(
        &mut self,
        facts: &ChatItemFacts,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let state = self.state.read(cx);
        let chat = state.chats.iter().find(|chat| chat.id == facts.chat_id);
        let session = chat
            .and_then(|chat| chat.harness_session_id.clone())
            .filter(|id| !id.trim().is_empty());
        let workspace = chat
            .and_then(|chat| chat.space_id.as_deref())
            .and_then(|id| state.space_row(id))
            .map(|space| space.path.clone());
        let rows = [
            ("chat", "Chat ID", Some(facts.chat_id.clone())),
            ("session", "Session ID", session),
            ("workspace", "Workspace", workspace),
        ];
        let mut block = div()
            .debug_selector(|| "chat-item-identifiers".into())
            .flex()
            .flex_col()
            .gap(px(3.0))
            .child(section_label(
                theme,
                section_icon("identifiers"),
                "Identifiers",
                "chat-item-identifiers-icon".into(),
            ));
        for (key, label, value) in rows {
            let Some(value) = value else { continue };
            let copied = self.copied_chat_identity.as_deref() == Some(key);
            let id: SharedString = format!("chat-item-id-{key}").into();
            let selector = id.to_string();
            let copy_value = value.clone();
            block = block.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .text_size(crate::typography::ui_rems(11.0))
                    .child(
                        div()
                            .flex_none()
                            .w(px(72.0))
                            .text_color(theme.text_faint)
                            .child(label),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.text_muted)
                            .child(SharedString::from(value)),
                    )
                    .child(
                        div()
                            .id(id)
                            .debug_selector(move || selector.clone())
                            .flex_none()
                            .px(px(4.0))
                            .rounded(px(3.0))
                            .cursor_pointer()
                            .text_color(theme.accent)
                            .hover(|el| el.bg(theme.element_hover))
                            .child(if copied { "Copied" } else { "Copy" })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.copy_identifier(key, copy_value.clone(), cx)
                            })),
                    ),
            );
        }
        block.into_any_element()
    }

    fn render_item_path(
        &mut self,
        item: &ItemSection,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut row = div().flex().flex_wrap().items_center().gap(px(4.0));
        for (ix, segment) in item.path.iter().enumerate() {
            let PathSegment { id, title } = segment;
            let space_id = item.space_id.clone();
            let item_id = id.clone();
            if ix > 0 {
                row = row.child(div().text_color(theme.text_faint).child("\u{203a}"));
            }
            row = row.child(
                div()
                    .id(("chat-item-path-segment", ix))
                    .debug_selector(move || format!("chat-item-path-segment-{ix}"))
                    .rounded(px(3.0))
                    .px(px(2.0))
                    .cursor_pointer()
                    .text_size(crate::typography::ui_rems(11.5))
                    .text_color(theme.text)
                    .hover(|el| el.bg(theme.element_hover))
                    .child(SharedString::from(if title.trim().is_empty() {
                        "Untitled".to_owned()
                    } else {
                        title.clone()
                    }))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.reveal_work_item_in_outline(&space_id, &item_id, window, cx)
                    })),
            );
        }
        row.into_any_element()
    }

    /// "Add to outline": show the chat in the Outline, where it can be moved
    /// under an item.
    fn reveal_chat_in_outline(
        &mut self,
        chat_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_overview(cx);
        let page = self.ensure_overview_page(cx);
        page.update(cx, |overview, cx| {
            overview.focus_outline_on_chat(None, window, cx)
        });
        let key = crate::work_outline::model::row_key::chat(chat_id);
        let outline = page.read(cx).outline_entity();
        outline.update(cx, |outline, cx| outline.reveal(&key, cx));
    }

    /// Show `item_id` in the Outline (expanding collapsed ancestors).
    pub(super) fn reveal_work_item_in_outline(
        &mut self,
        space_id: &str,
        item_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_overview(cx);
        let page = self.ensure_overview_page(cx);
        page.update(cx, |overview, cx| {
            overview.focus_outline_on_chat(None, window, cx)
        });
        let key = crate::work_outline::model::row_key::item(space_id, item_id);
        let outline = page.read(cx).outline_entity();
        outline.update(cx, |outline, cx| outline.reveal(&key, cx));
    }
}
