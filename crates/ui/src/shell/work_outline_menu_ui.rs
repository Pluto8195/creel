//! The Outline row context menu (`work_outline::outline_view::context_menu`
//! builds it): a Shell overlay like the sidebar Chat menu, with the same
//! card, rows, and exit animation.
//!
//! Keyboard: the card takes focus while open (`overlay_owns_keyboard`), so
//! Outline keys cannot fire underneath it. Up/Down (and Ctrl+P/N) move
//! between enabled entries, Home/End jump, Enter or Space runs one, Right
//! opens Set Status and Left returns, Escape (`on_key_down_capture`) or a
//! click outside dismisses it and hands focus back to the Outline.
//!
//! Running an entry closes the menu, refocuses the Outline, re-selects the
//! row, and then either dispatches the action the entry's key fires or
//! calls the helper its button uses. Nothing here re-implements an action.
use super::*;

use gpui::WeakEntity;

use crate::work_outline::hints::{self, HintKey, WithHint as _};
use crate::work_outline::outline_view::OutlineView;
use crate::work_outline::outline_view::context_menu::{
    MenuAction, MenuEntry, MenuKind, OutlineContextMenu,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MenuPage {
    Root,
    Status,
}

pub(super) struct WorkOutlineMenu {
    menu: OutlineContextMenu,
    position: Point<Pixels>,
    page: MenuPage,
    /// Keyboard cursor: an index into the current page's entries.
    active: Option<usize>,
    focus: FocusHandle,
    /// Claim `focus` at the next render, once the card is mounted (the
    /// Shell's focus recovery would otherwise take back an unmounted one).
    focus_pending: bool,
    /// The Outline's focus, restored on close.
    restore_focus: FocusHandle,
    outline: WeakEntity<OutlineView>,
}

impl WorkOutlineMenu {
    fn entries(&self) -> &[MenuEntry] {
        match self.page {
            MenuPage::Root => &self.menu.entries,
            MenuPage::Status => &self.menu.status_entries,
        }
    }

    fn enabled(&self) -> Vec<usize> {
        self.entries()
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.disabled.is_none())
            .map(|(ix, _)| ix)
            .collect()
    }

    /// Move the cursor by `delta` over enabled entries (wrapping).
    fn step(&mut self, delta: isize) {
        let enabled = self.enabled();
        let at = self
            .active
            .and_then(|active| enabled.iter().position(|ix| *ix == active));
        self.active = popover::menu_step(at, enabled.len(), delta).map(|pos| enabled[pos]);
    }
}

/// One rendered row: what the card draws and what tests read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MenuRow {
    pub id: &'static str,
    pub label: String,
    pub icon: &'static str,
    /// The live binding of the entry's action, `None` when unbound.
    pub keys: Option<String>,
    pub disabled: Option<String>,
    pub checked: bool,
    pub opens_page: bool,
    pub separator_before: bool,
    pub active: bool,
}

impl Shell {
    /// Show `menu` at `position`. `restore_focus` is the Outline's focus,
    /// returned to when the menu closes. A keyboard open puts the cursor on
    /// the first enabled entry.
    pub(crate) fn open_work_outline_menu(
        &mut self,
        mut menu: OutlineContextMenu,
        position: Point<Pixels>,
        outline: WeakEntity<OutlineView>,
        restore_focus: FocusHandle,
        from_keyboard: bool,
        cx: &mut Context<Self>,
    ) {
        if self.work_outline_modal.is_some() || self.command_palette.is_some() {
            return;
        }
        self.add_sidebar_chat_entries(&mut menu, cx);
        let mut state = WorkOutlineMenu {
            menu,
            position,
            page: MenuPage::Root,
            active: None,
            focus: cx.focus_handle(),
            focus_pending: true,
            restore_focus,
            outline,
        };
        if from_keyboard {
            state.step(1);
        }
        self.work_outline_menu.open(state);
        cx.notify();
    }

    /// Shift+F10 / the Menu key in the Outline: the selected row's menu,
    /// anchored below the row.
    pub(super) fn open_work_outline_menu_from_keyboard(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.overlay_owns_keyboard(cx) {
            return;
        }
        let Some(overview) = self.overview_page.clone() else {
            return;
        };
        let outline = overview.read(cx).outline_entity();
        let Some((menu, anchor)) = outline.read(cx).context_menu_for_selection(cx) else {
            return;
        };
        let focus = gpui::Focusable::focus_handle(outline.read(cx), cx);
        let position = anchor.unwrap_or_else(|| window.mouse_position());
        self.open_work_outline_menu(menu, position, outline.downgrade(), focus, true, cx);
    }

    /// The sidebar Chat menu's own actions, reused on Chat rows (only for
    /// Chats this device knows), and Apply's host blocker.
    fn add_sidebar_chat_entries(&self, menu: &mut OutlineContextMenu, cx: &App) {
        if menu.kind != MenuKind::Chat {
            return;
        }
        let Some(chat_id) = menu.chat_id.clone() else {
            return;
        };
        let state = self.state.read(cx);
        if let Some(apply) = menu
            .entries
            .iter_mut()
            .find(|entry| entry.action == MenuAction::ApplyInstructions(chat_id.clone()))
        {
            apply.disabled = super::instructions_ui::instruction_host_blocker(state, &chat_id);
        }
        let Some(chat) = state.chats.iter().find(|chat| chat.id == chat_id) else {
            return;
        };
        let closeable = crate::chat_closeout::is_closeout_candidate(chat.cwd.as_deref());
        let pinned = self.active_sidebar_pins(cx).contains(&chat_id);
        menu.entries.push(
            MenuEntry::new(
                "pin",
                if pinned { "Unpin" } else { "Pin" },
                icons::PIN,
                MenuAction::PinChat {
                    chat_id: chat_id.clone(),
                    pinned: !pinned,
                },
            )
            .separated(),
        );
        menu.entries.push(MenuEntry::new(
            "copy-identity",
            "Copy name + IDs",
            icons::COPY,
            MenuAction::CopyChatIdentity(chat_id.clone()),
        ));
        if closeable {
            menu.entries.push(MenuEntry::new(
                "close-out",
                "Close out worktree\u{2026}",
                icons::GIT_BRANCH,
                MenuAction::CloseOutChat(chat_id.clone()),
            ));
        }
        menu.entries.push(MenuEntry::new(
            "archive-chat",
            "Archive Chat",
            icons::ARCHIVE_MINIMALISTIC,
            MenuAction::ArchiveChat(chat_id),
        ));
    }

    /// The open menu's rows with their live bindings (keys are read from
    /// the keymap on every call, so a rebind shows at once).
    pub(super) fn work_outline_menu_rows(&self, cx: &App) -> Vec<MenuRow> {
        let Some(state) = self.work_outline_menu.get() else {
            return Vec::new();
        };
        let keymap = crate::settings::current(cx).keymap;
        state
            .entries()
            .iter()
            .enumerate()
            .map(|(ix, entry)| MenuRow {
                id: entry.id,
                label: entry.label.clone(),
                icon: entry.icon,
                keys: hints::hint_keys(&keymap, entry.hint),
                disabled: entry.disabled.clone(),
                checked: entry.checked,
                opens_page: entry.opens_page(),
                separator_before: entry.separator_before,
                active: state.active == Some(ix),
            })
            .collect()
    }

    /// Close without running anything; focus returns to the Outline unless
    /// a click outside already moved it.
    pub(super) fn dismiss_work_outline_menu(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.work_outline_menu.as_open() else {
            return;
        };
        if state.focus.is_focused(window) {
            window.focus(&state.restore_focus.clone(), cx);
        }
        self.begin_close_work_outline_menu(cx);
    }

    fn begin_close_work_outline_menu(&mut self, cx: &mut Context<Self>) {
        if self.work_outline_menu.begin_close() {
            popover::reap_popup(cx, |shell: &mut Self| &mut shell.work_outline_menu);
        }
        cx.notify();
    }

    fn work_outline_menu_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let keystroke = &event.keystroke;
        let Some(state) = self.work_outline_menu.open_mut() else {
            return;
        };
        match popover::classify_key(
            &keystroke.key,
            keystroke.modifiers.platform,
            keystroke.modifiers.control,
        ) {
            popover::MenuKey::Up => state.step(-1),
            popover::MenuKey::Down => state.step(1),
            popover::MenuKey::Escape => self.dismiss_work_outline_menu(window, cx),
            popover::MenuKey::Enter => {
                if let Some(ix) = state.active {
                    self.activate_work_outline_menu_entry(ix, window, cx);
                }
            }
            popover::MenuKey::Backspace => {
                if state.page == MenuPage::Status {
                    self.work_outline_menu_back(cx);
                }
            }
            popover::MenuKey::ModEnter | popover::MenuKey::Other => match keystroke.key.as_str() {
                "space" => {
                    if let Some(ix) = state.active {
                        self.activate_work_outline_menu_entry(ix, window, cx);
                    }
                }
                "home" => state.active = state.enabled().first().copied(),
                "end" => state.active = state.enabled().last().copied(),
                "right" => {
                    if let Some(ix) = state.active.filter(|ix| state.entries()[*ix].opens_page()) {
                        self.activate_work_outline_menu_entry(ix, window, cx);
                    }
                }
                "left" => {
                    if state.page == MenuPage::Status {
                        self.work_outline_menu_back(cx);
                    }
                }
                _ => return,
            },
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn work_outline_menu_back(&mut self, cx: &mut Context<Self>) {
        if let Some(state) = self.work_outline_menu.open_mut() {
            state.page = MenuPage::Root;
            state.active = state.menu.entries.iter().position(MenuEntry::opens_page);
            cx.notify();
        }
    }

    /// Run entry `ix` of the current page (disabled entries do nothing).
    pub(super) fn activate_work_outline_menu_entry(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.work_outline_menu.open_mut() else {
            return;
        };
        let Some(entry) = state.entries().get(ix).cloned() else {
            return;
        };
        if entry.disabled.is_some() {
            return;
        }
        match entry.action {
            MenuAction::StatusPage => {
                state.page = MenuPage::Status;
                // Past Back, onto the first status.
                state.active = state.enabled().get(1).copied();
                cx.notify();
                return;
            }
            MenuAction::Back => {
                self.work_outline_menu_back(cx);
                return;
            }
            _ => {}
        }
        let menu = state.menu.clone();
        let outline = state.outline.clone();
        let restore = state.restore_focus.clone();
        self.begin_close_work_outline_menu(cx);
        window.focus(&restore, cx);
        if let (Some(outline), Some(key)) = (outline.upgrade(), menu.row_key.as_deref()) {
            outline.update(cx, |outline, cx| outline.select(key, cx));
        }
        self.run_work_outline_menu_action(entry.action, &menu, &outline, window, cx);
    }

    fn run_work_outline_menu_action(
        &mut self,
        action: MenuAction,
        menu: &OutlineContextMenu,
        outline: &WeakEntity<OutlineView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Keyed entries: exactly what the key (or palette command) fires,
        // dispatched from the Outline's focus.
        if let Some(dispatch) = action.dispatch() {
            window.dispatch_action(dispatch, cx);
            return;
        }
        let target = menu.space_id.clone().zip(menu.item_id.clone());
        match action {
            MenuAction::SetStatus(status) => {
                self.with_menu_details(target, window, cx, move |details, _, cx| {
                    details.set_status(status, cx)
                })
            }
            MenuAction::AddInstruction => {
                self.with_menu_details(target, window, cx, |details, window, cx| {
                    details.begin_instruction_draft(None, window, cx)
                })
            }
            MenuAction::Archive => self.with_menu_details(target, window, cx, |details, _, cx| {
                details.request_archive(cx)
            }),
            MenuAction::Restore => {
                self.with_menu_details(target, window, cx, |details, _, cx| details.restore(cx))
            }
            MenuAction::SpaceInstructions => {
                self.with_menu_details(target, window, cx, |details, _, cx| {
                    details.instructions_open = true;
                    cx.notify();
                })
            }
            MenuAction::ExecutionSetup => {
                self.with_menu_details(target, window, cx, |details, _, cx| {
                    details.execution_open = true;
                    cx.notify();
                })
            }
            MenuAction::RemoveFromWorkItem(chat_id) => {
                self.with_menu_details(target, window, cx, move |details, _, cx| {
                    details.disassociate(chat_id, cx)
                })
            }
            MenuAction::CopyPath(path) => {
                cx.write_to_clipboard(ClipboardItem::new_string(path));
                if let Some(outline) = outline.upgrade() {
                    outline.update(cx, |outline, cx| outline.show_notice("Path copied", cx));
                }
            }
            MenuAction::CompareInstructions(chat_id) => self.open_instruction_compare(&chat_id, cx),
            MenuAction::ApplyInstructions(chat_id) => self.apply_chat_instructions(&chat_id, cx),
            MenuAction::PinChat { chat_id, pinned } => self.set_chat_pinned(chat_id, pinned, cx),
            MenuAction::CopyChatIdentity(chat_id) => self.copy_chat_identity(&chat_id, true, cx),
            MenuAction::CloseOutChat(chat_id) => self.open_chat_closeout(chat_id, cx),
            MenuAction::ArchiveChat(chat_id) => self.archive_chat(chat_id, cx),
            MenuAction::Outline(_)
            | MenuAction::Command(_)
            | MenuAction::StatusPage
            | MenuAction::Back => {}
        }
    }

    fn with_menu_details(
        &mut self,
        target: Option<(String, String)>,
        window: &Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(
            &mut crate::work_outline::details_panel::WorkItemDetails,
            &mut Window,
            &mut Context<crate::work_outline::details_panel::WorkItemDetails>,
        ) + 'static,
    ) {
        let (Some((space_id, item_id)), Some(overview)) = (target, self.overview_page.clone())
        else {
            return;
        };
        overview.update(cx, |overview, cx| {
            overview.with_work_item_details(space_id, item_id, window, cx, then)
        });
    }

    pub(super) fn render_work_outline_menu(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if let Some(state) = self.work_outline_menu.open_mut()
            && std::mem::take(&mut state.focus_pending)
        {
            window.focus(&state.focus, cx);
        }
        let state = self.work_outline_menu.get()?;
        let position = state.position;
        let focus = state.focus.clone();
        let page = state.page;
        let closing = self.work_outline_menu.closing_since();
        let rows = self.work_outline_menu_rows(cx);
        let theme = Theme::of(cx).for_popup();
        let mut card = popover::popover_card(&theme)
            .w(px(264.0))
            .flex()
            .flex_col()
            .debug_selector(|| "outline-menu".into())
            .track_focus(&focus)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                this.work_outline_menu_key(event, window, cx)
            }))
            .on_mouse_down_out(
                cx.listener(|this, _, window, cx| this.dismiss_work_outline_menu(window, cx)),
            );
        for (ix, row) in rows.into_iter().enumerate() {
            if row.separator_before {
                card = card.child(popover::menu_separator());
            }
            card = card.child(menu_row(ix, row, page, &theme, cx));
        }
        Some(popover::menu_at(
            "work-outline-context-menu",
            position,
            card.into_any_element(),
            closing,
        ))
    }
}

fn menu_row(
    ix: usize,
    row: MenuRow,
    page: MenuPage,
    theme: &Theme,
    cx: &mut Context<Shell>,
) -> AnyElement {
    let selector = format!("outline-menu-{}", row.id);
    let fade = format!("outline-menu-{page:?}-{}", row.id);
    let glyph = if row.checked {
        icon(icons::CHECK)
            .size(px(14.0))
            .text_color(theme.accent)
            .into_any_element()
    } else if row.icon.is_empty() {
        div().into_any_element()
    } else {
        icon(row.icon)
            .size(px(16.0))
            .text_color(theme.text_muted)
            .into_any_element()
    };
    let keys = row.keys.map(|keys| {
        let selector = format!("outline-menu-key-{}", row.id);
        div()
            .flex_none()
            .debug_selector(move || selector.clone())
            .child(popover::kbd_hint(theme, &keys))
    });
    let row_el = popover::menu_row_nav(theme, false, row.active, fade)
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .child(
            div()
                .w(px(16.0))
                .flex_none()
                .flex()
                .justify_center()
                .child(glyph),
        )
        .child(div().flex_1().min_w_0().truncate().child(row.label))
        .children(keys)
        .when(row.opens_page, |el| {
            el.child(
                icon(icons::ALT_ARROW_RIGHT)
                    .size(px(14.0))
                    .text_color(theme.text_muted),
            )
        });
    match row.disabled {
        Some(reason) => row_el
            .bg(gpui::transparent_black())
            .text_color(theme.text_faint)
            .cursor_default()
            .hint(reason, HintKey::None)
            .into_any_element(),
        None => row_el
            .on_click(cx.listener(move |this, _, window, cx| {
                this.activate_work_outline_menu_entry(ix, window, cx);
                cx.stop_propagation();
            }))
            .into_any_element(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::work_outline::WorkOutlineActions;
    use crate::work_outline::command::WorkOutlineCommand;
    use crate::work_outline::hints::key_label;
    use crate::work_outline::model::row_key;
    use crate::work_outline::store::WorkOutlineSnapshot;
    use crate::work_outline::store::fixture_ids::*;

    type Calls = Arc<Mutex<Vec<serde_json::Value>>>;

    fn reply_ok(_: &serde_json::Value) -> Result<serde_json::Value, zeron_rpc::RpcError> {
        Ok(serde_json::json!({}))
    }

    /// A Shell on the Outline over `snapshot` (Space `s1`), with a scripted
    /// engine that records every Work Outline call.
    fn menu_shell<'a>(
        cx: &'a mut gpui::TestAppContext,
        dir: &std::path::Path,
        snapshot: WorkOutlineSnapshot,
    ) -> (
        Entity<Shell>,
        Entity<OutlineView>,
        Calls,
        &'a mut gpui::VisualTestContext,
    ) {
        let settings_dir = dir.join("settings");
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
            settings::init(settings::UiSettings::default(), &settings_dir, cx);
        });
        let data_dir = dir.to_path_buf();
        let (shell, cx) = cx.add_window_view(move |_, cx| {
            let state = cx.new(|_| AppState::new());
            state.update(cx, |state, _| {
                state.connection = crate::state::ConnectionStatus::Ready;
                state.workspace_scope = Some(WorkspaceScope::Local);
                state.auth = Some(zeron_proto::AuthState::SignedOut);
            });
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: data_dir.clone(),
                    ipc_port: 0,
                    edge_url: "http://127.0.0.1:1".into(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: zeron_proto::HarnessId::Mock,
                },
                cx,
            )
        });
        let calls: Calls = Arc::default();
        let sink = calls.clone();
        let actions = WorkOutlineActions::scripted(Arc::new(move |_, params| {
            sink.lock().unwrap().push(params.clone());
            reply_ok(&params)
        }));
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                state.spaces = vec![
                    serde_json::from_value(serde_json::json!({
                        "id": "s1", "deviceId": "d", "path": "/work/alpha",
                        "createdAt": "2026-01-01T00:00:00Z"
                    }))
                    .unwrap(),
                ];
                state.chats = [CHAT_API, CHAT_AUTH, CHAT_UI, CHAT_LOOSE_A]
                    .iter()
                    .map(|id| {
                        serde_json::from_value(serde_json::json!({
                            "id": id, "deviceId": "d", "archived": false,
                            "createdAt": "2026-01-01T00:00:00Z", "spaceId": "s1",
                            "title": format!("Chat {id}"),
                        }))
                        .unwrap()
                    })
                    .collect();
                state.set_work_outline_visible(vec!["s1".into()]);
                assert!(state.apply_work_outline_frame("s1", snapshot));
                state.work_outline_actions_override = Some(actions);
            })
        });
        shell.update_in(cx, |shell, window, cx| {
            shell.run_work_outline_command(WorkOutlineCommand::OpenOutline, window, cx)
        });
        redraw(cx);
        let outline = shell.read_with(cx, |shell, cx| {
            shell
                .overview_page
                .clone()
                .unwrap()
                .read(cx)
                .outline_entity()
        });
        (shell, outline, calls, cx)
    }

    fn redraw(cx: &mut gpui::VisualTestContext) {
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear());
    }

    fn selector(text: String) -> &'static str {
        Box::leak(text.into_boxed_str())
    }

    fn right_click(cx: &mut gpui::VisualTestContext, key: &str) {
        let row = cx
            .debug_bounds(selector(format!("outline-row-{key}")))
            .unwrap_or_else(|| panic!("row {key} is shown"));
        cx.simulate_mouse_down(row.center(), MouseButton::Right, gpui::Modifiers::default());
        cx.run_until_parked();
        redraw(cx);
    }

    fn click(cx: &mut gpui::VisualTestContext, id: &str) {
        let entry = cx
            .debug_bounds(selector(format!("outline-menu-{id}")))
            .unwrap_or_else(|| panic!("entry {id} is shown"));
        cx.simulate_click(entry.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        redraw(cx);
    }

    fn press(cx: &mut gpui::VisualTestContext, keys: &str) {
        cx.simulate_keystrokes(keys);
        cx.run_until_parked();
        redraw(cx);
    }

    fn rows(shell: &Entity<Shell>, cx: &mut gpui::VisualTestContext) -> Vec<MenuRow> {
        shell.read_with(cx, |shell, cx| shell.work_outline_menu_rows(cx))
    }

    fn row(shell: &Entity<Shell>, cx: &mut gpui::VisualTestContext, id: &str) -> MenuRow {
        rows(shell, cx)
            .into_iter()
            .find(|row| row.id == id)
            .unwrap_or_else(|| panic!("no {id} entry"))
    }

    fn ids(shell: &Entity<Shell>, cx: &mut gpui::VisualTestContext) -> Vec<&'static str> {
        rows(shell, cx).into_iter().map(|row| row.id).collect()
    }

    fn active(shell: &Entity<Shell>, cx: &mut gpui::VisualTestContext) -> Option<&'static str> {
        rows(shell, cx)
            .into_iter()
            .find(|row| row.active)
            .map(|row| row.id)
    }

    fn menu_open(shell: &Entity<Shell>, cx: &mut gpui::VisualTestContext) -> bool {
        shell.read_with(cx, |shell, _| shell.work_outline_menu.is_open())
    }

    fn menu_focused(shell: &Entity<Shell>, cx: &mut gpui::VisualTestContext) -> bool {
        cx.update(|window, cx| {
            shell
                .read(cx)
                .work_outline_menu
                .as_open()
                .is_some_and(|menu| menu.focus.is_focused(window))
        })
    }

    /// Unmount a closed menu whose exit animation would still be playing
    /// (it occludes the rows underneath; the reap measures wall time).
    fn settle(shell: &Entity<Shell>, cx: &mut gpui::VisualTestContext) {
        shell.update(cx, |shell, _| {
            assert!(!shell.work_outline_menu.is_open(), "closed first");
            shell.work_outline_menu = popover::Popup::default();
        });
        redraw(cx);
    }

    fn selected(outline: &Entity<OutlineView>, cx: &mut gpui::VisualTestContext) -> Option<String> {
        outline.read_with(cx, |outline, _| outline.selected_key().map(str::to_owned))
    }

    fn outline_focused(outline: &Entity<OutlineView>, cx: &mut gpui::VisualTestContext) -> bool {
        cx.update(|window, cx| {
            gpui::Focusable::focus_handle(outline.read(cx), cx).is_focused(window)
        })
    }

    const ITEM_ENTRIES: [&str; 17] = [
        "open-details",
        "start-chat",
        "rename",
        "new-sibling",
        "new-child",
        "move-up",
        "move-down",
        "indent",
        "outdent",
        "move-to",
        "set-status",
        "associate",
        "effective-instructions",
        "add-instruction",
        "copy-path",
        "archive",
        "restore",
    ];

    /// Right-click selects the row and opens its menu; invalid actions are
    /// greyed out with the reason; Escape closes and hands focus back.
    #[gpui::test]
    fn right_click_selects_the_row_and_opens_the_item_menu(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let fixture = crate::work_outline::store::fixture_snapshot("s1");
        let (shell, outline, calls, cx) = menu_shell(cx, dir.path(), fixture);

        let api = row_key::item("s1", API);
        right_click(cx, &api);
        assert_eq!(
            selected(&outline, cx),
            Some(api.clone()),
            "right-click selects"
        );
        assert!(menu_open(&shell, cx));
        assert!(
            cx.debug_bounds("outline-menu").is_some(),
            "the card renders"
        );
        assert!(menu_focused(&shell, cx), "the card owns the keyboard");
        assert_eq!(ids(&shell, cx), ITEM_ENTRIES);
        shell.update(cx, |shell, cx| assert!(shell.overlay_owns_keyboard(cx)));
        assert!(!outline_focused(&outline, cx), "the menu owns the keyboard");

        // API is Launch v2's first child: nothing above to move before or
        // indent under; it can move down and outdent. It is live.
        let disabled = |id: &str, cx: &mut gpui::VisualTestContext| row(&shell, cx, id).disabled;
        assert_eq!(
            disabled("move-up", cx).as_deref(),
            Some("Already the first item at this level")
        );
        assert_eq!(
            disabled("indent", cx).as_deref(),
            Some("No item above to indent under")
        );
        assert_eq!(disabled("move-down", cx), None);
        assert_eq!(disabled("outdent", cx), None);
        assert_eq!(
            disabled("restore", cx).as_deref(),
            Some("This work item is not archived")
        );
        assert_eq!(disabled("archive", cx), None);
        assert!(row(&shell, cx, "set-status").opens_page);

        // A greyed-out entry does nothing.
        click(cx, "move-up");
        assert!(menu_open(&shell, cx), "disabled entries keep the menu open");
        assert!(calls.lock().unwrap().is_empty());

        // Escape closes and refocuses the Outline.
        press(cx, "escape");
        assert!(!menu_open(&shell, cx));
        assert!(
            outline_focused(&outline, cx),
            "focus returns to the Outline"
        );
        shell.update(cx, |shell, cx| assert!(!shell.overlay_owns_keyboard(cx)));
        settle(&shell, cx);

        // A top-level item cannot outdent.
        right_click(cx, &row_key::item("s1", LAUNCH));
        assert_eq!(selected(&outline, cx), Some(row_key::item("s1", LAUNCH)));
        assert_eq!(
            row(&shell, cx, "outdent").disabled.as_deref(),
            Some("Already at the top level")
        );
        assert_eq!(row(&shell, cx, "move-down").disabled, None);

        // A click outside dismisses it too.
        cx.simulate_mouse_down(
            gpui::point(px(2.0), px(2.0)),
            MouseButton::Left,
            gpui::Modifiers::default(),
        );
        cx.run_until_parked();
        assert!(!menu_open(&shell, cx));
    }

    /// Chat rows, the Space root, and the Unassociated group get their own
    /// menus; Chat rows reuse the sidebar Chat menu's actions.
    #[gpui::test]
    fn chat_space_root_and_unassociated_menus(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let fixture = crate::work_outline::instructions::fixture::snapshot("s1");
        let (shell, outline, _calls, cx) = menu_shell(cx, dir.path(), fixture);

        // chat-api: associated with API, instructions drifted.
        let chat = row_key::chat(CHAT_API);
        right_click(cx, &chat);
        assert_eq!(selected(&outline, cx), Some(chat));
        assert_eq!(
            ids(&shell, cx),
            [
                "open-chat",
                "rename",
                "associate",
                "remove-from-item",
                "compare-instructions",
                "apply-instructions",
                "pin",
                "copy-identity",
                "archive-chat",
            ]
        );
        assert_eq!(
            row(&shell, cx, "associate").label,
            "Move to Work Item\u{2026}"
        );
        assert_eq!(row(&shell, cx, "pin").label, "Pin");
        press(cx, "escape");
        settle(&shell, cx);

        // An unassociated Chat: Associate, nothing to remove or compare.
        right_click(cx, &row_key::chat(CHAT_LOOSE_A));
        let chat_ids = ids(&shell, cx);
        assert_eq!(
            row(&shell, cx, "associate").label,
            "Associate with Work Item\u{2026}"
        );
        assert!(!chat_ids.contains(&"remove-from-item"));
        assert!(!chat_ids.contains(&"compare-instructions"));
        press(cx, "escape");
        settle(&shell, cx);

        // The single-Space root entry.
        let entry = cx.debug_bounds("outline-space-root-entry").unwrap();
        cx.simulate_mouse_down(
            entry.center(),
            MouseButton::Right,
            gpui::Modifiers::default(),
        );
        cx.run_until_parked();
        redraw(cx);
        assert_eq!(
            ids(&shell, cx),
            [
                "new-item",
                "space-instructions",
                "execution",
                "required-actions",
                "shortcuts"
            ]
        );
        assert_eq!(row(&shell, cx, "shortcuts").keys.as_deref(), Some("?"));
        // Required Actions runs the palette command.
        click(cx, "required-actions");
        assert!(!menu_open(&shell, cx));
        settle(&shell, cx);
        assert!(cx.debug_bounds("required-actions-panel").is_some());

        // The Unassociated group: nothing to offer without a selected Chat,
        // then Associate for the app's selected Chat.
        let group = row_key::unassociated(Some("s1"));
        right_click(cx, &group);
        assert_eq!(selected(&outline, cx), Some(group.clone()));
        assert!(!menu_open(&shell, cx), "no menu without a selected Chat");
        settle(&shell, cx);
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                state.selected_chat = Some(CHAT_LOOSE_A.into())
            })
        });
        right_click(cx, &group);
        assert_eq!(ids(&shell, cx), ["associate"]);
        assert_eq!(
            row(&shell, cx, "associate").label,
            format!("Associate \u{201c}Chat {CHAT_LOOSE_A}\u{201d}\u{2026}")
        );
    }

    /// Each entry shows its action's current binding and follows a rebind.
    #[gpui::test]
    fn entries_show_live_bindings_and_follow_a_rebind(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let fixture = crate::work_outline::store::fixture_snapshot("s1");
        let (shell, _outline, _calls, cx) = menu_shell(cx, dir.path(), fixture);
        right_click(cx, &row_key::item("s1", API));

        let keys = |id: &str, cx: &mut gpui::VisualTestContext| row(&shell, cx, id).keys;
        assert_eq!(keys("open-details", cx), Some(key_label("enter")));
        assert_eq!(keys("rename", cx), Some(key_label("f2")));
        assert_eq!(keys("new-child", cx), Some(key_label("shift-n")));
        assert_eq!(keys("move-down", cx), Some(key_label("shift-down")));
        assert_eq!(keys("outdent", cx), Some(key_label("shift-left")));
        // Unbound commands and helper entries show no key.
        assert_eq!(keys("start-chat", cx), None);
        assert_eq!(keys("copy-path", cx), None);
        assert!(cx.debug_bounds("outline-menu-key-start-chat").is_none());
        assert!(cx.debug_bounds("outline-menu-key-rename").is_some());

        cx.update(|_, cx| {
            settings::update(SavePolicy::Debounced, cx, |s| {
                s.keymap
                    .set(ShortcutId::StartChatForWorkItem, "mod-alt-k".into())
            });
        });
        redraw(cx);
        assert_eq!(keys("start-chat", cx), Some(key_label("mod-alt-k")));
        assert!(cx.debug_bounds("outline-menu-key-start-chat").is_some());

        cx.update(|_, cx| {
            settings::update(SavePolicy::Debounced, cx, |s| {
                s.keymap
                    .set(ShortcutId::StartChatForWorkItem, String::new())
            });
        });
        redraw(cx);
        assert_eq!(keys("start-chat", cx), None);
    }

    /// An entry runs the very action its key runs: Move Up from the menu
    /// sends the same `moveWorkItem` as Shift+Up; Move To opens the picker
    /// the palette command opens.
    #[gpui::test]
    fn choosing_an_entry_dispatches_the_keys_action(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let fixture = crate::work_outline::store::fixture_snapshot("s1");
        let (shell, outline, calls, cx) = menu_shell(cx, dir.path(), fixture);

        right_click(cx, &row_key::item("s1", UI));
        click(cx, "move-up");
        assert!(!menu_open(&shell, cx));
        let call = calls.lock().unwrap()[0].clone();
        assert_eq!(call["op"], "moveWorkItem", "{call}");
        assert_eq!(call["workItemId"], UI);
        assert_eq!(call["destinationParentWorkItemId"], LAUNCH);
        assert_eq!(
            call["placement"],
            serde_json::json!({"type": "before", "siblingWorkItemId": API})
        );
        assert_eq!(selected(&outline, cx), Some(row_key::item("s1", UI)));
        assert!(outline_focused(&outline, cx));
        // The same optimistic path as the key: UI shows above API before
        // any engine frame does.
        outline.read_with(cx, |outline, _| {
            let model = outline.model();
            let position = |id| model.position(&row_key::item("s1", id)).unwrap();
            assert!(position(UI) < position(API), "shown at once");
            assert_eq!(outline.optimistic_len(), 1);
        });
        settle(&shell, cx);

        // Move To… runs the Move command: its picker opens.
        right_click(cx, &row_key::item("s1", HOUSEKEEPING));
        click(cx, "move-to");
        outline.read_with(cx, |outline, _| {
            assert!(outline.is_editing(), "picker open")
        });
    }

    /// Shift+F10 opens the selected row's menu with the cursor on the first
    /// enabled entry; arrows skip greyed-out entries and wrap; Right / Left
    /// enter and leave Set Status; Enter runs the entry.
    #[gpui::test]
    fn keyboard_opens_and_navigates_the_menu(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let fixture = crate::work_outline::store::fixture_snapshot("s1");
        let (shell, outline, calls, cx) = menu_shell(cx, dir.path(), fixture);
        let housekeeping = row_key::item("s1", HOUSEKEEPING);
        outline.update(cx, |outline, cx| outline.select(&housekeeping, cx));
        cx.update(|window, cx| {
            let focus = gpui::Focusable::focus_handle(outline.read(cx), cx);
            window.focus(&focus, cx);
        });
        redraw(cx);

        press(cx, "shift-f10");
        assert!(menu_open(&shell, cx), "Shift+F10 opens the menu");
        assert!(menu_focused(&shell, cx), "the card owns the keyboard");
        assert_eq!(active(&shell, cx), Some("open-details"));
        press(cx, "down");
        assert_eq!(active(&shell, cx), Some("start-chat"));
        press(cx, "up");
        press(cx, "up");
        // Wraps to the last enabled entry (Restore is greyed out).
        assert_eq!(active(&shell, cx), Some("archive"));
        press(cx, "home");
        assert_eq!(active(&shell, cx), Some("open-details"));
        // Housekeeping is the last top-level live item: Move Down and
        // Outdent are skipped on the way to Set Status.
        for _ in 0..ITEM_ENTRIES.len() {
            if active(&shell, cx) == Some("set-status") {
                break;
            }
            press(cx, "down");
            assert_ne!(active(&shell, cx), Some("move-down"));
            assert_ne!(active(&shell, cx), Some("outdent"));
        }
        assert_eq!(active(&shell, cx), Some("set-status"));

        press(cx, "right");
        assert_eq!(
            ids(&shell, cx),
            [
                "status-back",
                "status-pending",
                "status-active",
                "status-completed"
            ]
        );
        assert_eq!(active(&shell, cx), Some("status-pending"));
        assert!(row(&shell, cx, "status-pending").checked, "current status");
        press(cx, "left");
        assert_eq!(active(&shell, cx), Some("set-status"));
        press(cx, "right");
        press(cx, "down");
        assert_eq!(active(&shell, cx), Some("status-active"));
        press(cx, "enter");
        assert!(!menu_open(&shell, cx));
        let calls = calls.lock().unwrap().clone();
        let update = calls
            .iter()
            .find(|call| call["op"] == "updateWorkItem")
            .expect("status update sent");
        assert_eq!(update["workItemId"], HOUSEKEEPING);
        assert!(update.to_string().contains("\"active\""), "{update}");
        // Through Work Item Details, which now show the item.
        let overview = shell.read_with(cx, |shell, _| shell.overview_page.clone().unwrap());
        overview.read_with(cx, |overview, _| assert!(overview.has_work_item_details()));
        settle(&shell, cx);

        // The Menu key opens it as well; Escape closes it.
        cx.update(|window, cx| {
            let focus = gpui::Focusable::focus_handle(outline.read(cx), cx);
            window.focus(&focus, cx);
        });
        redraw(cx);
        press(cx, "menu");
        assert!(menu_open(&shell, cx), "the Menu key opens the menu");
        press(cx, "escape");
        assert!(!menu_open(&shell, cx));
    }

    /// The live flow, end to end in the real window composition: a Shell on
    /// the Overview in Outline mode (keymap from `Shell::new`), one Space
    /// whose top level holds several items, a real left click on the second
    /// top-level row, then Shift+Up. The row moves at once (optimistic) and
    /// one `moveWorkItem` goes out; Shift+Down / Shift+Right / Shift+Left
    /// follow the same path.
    #[gpui::test]
    fn click_then_shift_arrows_restructure_top_level_items_in_the_shell(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let mut snapshot = crate::work_outline::store::fixture_snapshot("s1");
        // Only top-level items (parent None), as in the reported case.
        snapshot.root_work_item_ids.retain(|id| id != RETIRED);
        let (_shell, outline, calls, cx) = menu_shell(cx, dir.path(), snapshot);
        let launch = row_key::item("s1", LAUNCH);
        let housekeeping = row_key::item("s1", HOUSEKEEPING);
        let position = |outline: &Entity<OutlineView>, cx: &mut gpui::VisualTestContext, key| {
            outline.read_with(cx, |outline, _| outline.model().position(key))
        };
        let row_click = |cx: &mut gpui::VisualTestContext, key: &str| {
            let at = cx
                .debug_bounds(selector(format!("outline-row-{key}")))
                .unwrap_or_else(|| panic!("row {key} is shown"))
                .center();
            cx.simulate_click(at, gpui::Modifiers::default());
            cx.run_until_parked();
            redraw(cx);
        };
        assert!(
            position(&outline, cx, &launch) < position(&outline, cx, &housekeeping),
            "Launch starts above Housekeeping"
        );

        row_click(cx, &housekeeping);
        assert_eq!(selected(&outline, cx), Some(housekeeping.clone()));
        assert!(outline_focused(&outline, cx), "a click keeps focus in the tree");

        press(cx, "shift-up");
        assert!(
            position(&outline, cx, &housekeeping) < position(&outline, cx, &launch),
            "Shift+Up moves Housekeeping above Launch at once"
        );
        {
            let calls = calls.lock().unwrap();
            let moves: Vec<_> = calls
                .iter()
                .filter(|call| call["op"] == "moveWorkItem")
                .collect();
            assert_eq!(moves.len(), 1, "{calls:?}");
            assert_eq!(moves[0]["workItemId"], HOUSEKEEPING);
            assert_eq!(moves[0]["destinationParentWorkItemId"], serde_json::Value::Null);
            assert_eq!(
                moves[0]["placement"],
                serde_json::json!({"type": "before", "siblingWorkItemId": LAUNCH})
            );
        }
        assert_eq!(selected(&outline, cx), Some(housekeeping.clone()));

        // Shift+Down moves it back below Launch.
        press(cx, "shift-down");
        assert!(
            position(&outline, cx, &launch) < position(&outline, cx, &housekeeping),
            "Shift+Down moves Housekeeping back below Launch"
        );
        let moves = |calls: &Calls| {
            calls
                .lock()
                .unwrap()
                .iter()
                .filter(|call| call["op"] == "moveWorkItem")
                .count()
        };
        assert_eq!(moves(&calls), 2);

        // Shift+Right indents under Launch; Shift+Left outdents again.
        press(cx, "shift-right");
        assert_eq!(moves(&calls), 3);
        press(cx, "shift-left");
        assert_eq!(moves(&calls), 4);
    }
}
