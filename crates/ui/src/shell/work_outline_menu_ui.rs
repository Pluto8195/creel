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
            MenuAction::RecentChanges => {
                if let Some(outline) = outline.upgrade() {
                    outline.update(cx, |outline, cx| {
                        if let Err(notice) = outline.open_recent_changes(window, cx) {
                            outline.show_notice(notice, cx);
                        }
                    });
                }
            }
            MenuAction::CopyPath(path) => {
                cx.write_to_clipboard(ClipboardItem::new_string(path));
                if let Some(outline) = outline.upgrade() {
                    outline.update(cx, |outline, cx| outline.show_notice("Path copied", cx));
                }
            }
            MenuAction::CompareInstructions(chat_id) => self.open_instruction_compare(&chat_id, cx),
            MenuAction::ApplyInstructions(chat_id) => {
                // A fresh-session warning needs a visible surface to confirm
                // on: open Compare, where it shows with Apply / Cancel.
                if super::instructions_ui::fresh_session_apply_warning(
                    self.state.read(cx),
                    &chat_id,
                )
                .is_some()
                {
                    self.open_instruction_compare(&chat_id, cx);
                }
                self.apply_chat_instructions(&chat_id, cx)
            }
            MenuAction::PinChat { chat_id, pinned } => self.set_chat_pinned(chat_id, pinned, cx),
            MenuAction::CopyChatIdentity(chat_id) => self.copy_chat_identity(&chat_id, true, cx),
            MenuAction::CloseOutChat(chat_id) => self.open_chat_closeout(chat_id, cx),
            MenuAction::ArchiveChat(chat_id) => self.archive_chat(chat_id, cx),
            MenuAction::HideSpace(space_id) => {
                Self::set_outline_space_hidden(outline, &space_id, true, cx)
            }
            MenuAction::UnhideSpace(space_id) => {
                Self::set_outline_space_hidden(outline, &space_id, false, cx)
            }
            // The sidebar's own flows: the same dialog and confirmation.
            MenuAction::RenameSpace(space_id) => self.open_rename_space(space_id, cx),
            MenuAction::DeleteSpace(space_id) => {
                self.delete_space_confirm = Some(space_id);
                cx.notify();
            }
            MenuAction::ProjectAccount(space_id, harness) => {
                self.open_project_account_picker(space_id, harness, cx)
            }
            MenuAction::Linear(action) => self.run_linear_menu_action(action, window, cx),
            MenuAction::Github(action) => self.run_github_menu_action(action, window, cx),
            MenuAction::Outline(_)
            | MenuAction::Command(_)
            | MenuAction::StatusPage
            | MenuAction::Back => {}
        }
    }

    fn set_outline_space_hidden(
        outline: &WeakEntity<OutlineView>,
        space_id: &str,
        hidden: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(outline) = outline.upgrade() {
            outline.update(cx, |outline, cx| {
                outline.set_space_hidden(space_id, hidden, cx)
            });
        }
    }

    /// Archive session (Shift+Cmd+A) with the Outline tree focused: acts on
    /// the selected row instead of the open Chat. A Chat row is archived the
    /// way the sidebar does it (the selection moves on to the next row); a
    /// work item opens its Archive dialog (the descendant choice); other
    /// rows do nothing. `false` when the tree is not focused, so the
    /// shortcut keeps its Chat behavior everywhere else.
    pub(super) fn archive_selected_outline_row(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(overview) = self.overview_page.clone() else {
            return false;
        };
        if !matches!(self.route, Route::Overview)
            || !overview.read(cx).outline_tree_focused(window, cx)
        {
            return false;
        }
        let outline = overview.read(cx).outline_entity();
        let Some(row) = outline.read(cx).selected_row().cloned() else {
            return true;
        };
        match row.kind {
            crate::work_outline::model::OutlineRowKind::Chat(chat)
                if !chat.dangling && !chat.chat.archived =>
            {
                outline.update(cx, |outline, cx| outline.select_past(&row.key, cx));
                self.archive_chat(chat.chat.id, cx);
            }
            crate::work_outline::model::OutlineRowKind::Item(item) if !item.archived => {
                self.with_menu_details(
                    Some((item.space_id, item.item_id)),
                    window,
                    cx,
                    |details, _, cx| details.request_archive(cx),
                );
            }
            _ => {}
        }
        true
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

    fn reply_ok(_: &serde_json::Value) -> Result<serde_json::Value, mast_rpc::RpcError> {
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
            // The chat route renders the transcript too.
            crate::history::init(
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                cx,
            );
        });
        let data_dir = dir.to_path_buf();
        let (shell, cx) = cx.add_window_view(move |_, cx| {
            let state = cx.new(|_| AppState::new());
            state.update(cx, |state, _| {
                state.connection = crate::state::ConnectionStatus::Ready;
                state.workspace_scope = Some(WorkspaceScope::Local);
                state.auth = Some(mast_proto::AuthState::SignedOut);
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
                    default_harness: mast_proto::HarnessId::Mock,
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
        // These tests drive Chat rows; compact rows have their own tests.
        outline.update(cx, |outline, cx| outline.set_compact_single_chat(false, cx));
        redraw(cx);
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

    const ITEM_ENTRIES: [&str; 21] = [
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
        "group-into",
        "set-status",
        "associate",
        "effective-instructions",
        "add-instruction",
        "copy-path",
        "archive",
        "restore",
        "undo",
        "redo",
        "recent-changes",
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
                "open-full-chat",
                "rename",
                "associate",
                "move-to-previous-item",
                "move-to-next-item",
                "move-to-parent-item",
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
        // Open Full Chat shows its default key.
        assert_eq!(
            row(&shell, cx, "open-full-chat").keys,
            Some(key_label(ShortcutId::OpenFullChat.default_combo()))
        );
        // The Chat moves show the restructuring keys that run them.
        let keys = |id: &str, cx: &mut gpui::VisualTestContext| row(&shell, cx, id).keys;
        assert_eq!(
            keys("move-to-previous-item", cx),
            Some(key_label("shift-up"))
        );
        assert_eq!(keys("move-to-next-item", cx), Some(key_label("shift-down")));
        assert_eq!(
            keys("move-to-parent-item", cx),
            Some(key_label("shift-left"))
        );
        assert_eq!(
            row(&shell, cx, "move-to-parent-item").label,
            "Move to Parent Item"
        );
        for id in [
            "move-to-previous-item",
            "move-to-next-item",
            "move-to-parent-item",
        ] {
            assert_eq!(row(&shell, cx, id).disabled, None, "{id}");
        }
        press(cx, "escape");
        settle(&shell, cx);

        // An unassociated Chat: Associate, nothing to remove or compare;
        // it can only move up into the last work item.
        right_click(cx, &row_key::chat(CHAT_LOOSE_A));
        let chat_ids = ids(&shell, cx);
        assert_eq!(
            row(&shell, cx, "associate").label,
            "Associate with Work Item\u{2026}"
        );
        assert!(!chat_ids.contains(&"remove-from-item"));
        assert!(!chat_ids.contains(&"compare-instructions"));
        assert_eq!(row(&shell, cx, "move-to-previous-item").disabled, None);
        assert_eq!(
            row(&shell, cx, "move-to-next-item").disabled.as_deref(),
            Some("No work item below")
        );
        assert_eq!(
            row(&shell, cx, "move-to-parent-item").disabled.as_deref(),
            Some("Not in a work item")
        );
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
                "undo",
                "redo",
                "recent-changes",
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
        assert_eq!(keys("open-details", cx), Some(key_label("mod-i")));
        assert_eq!(keys("rename", cx), Some(key_label("f2")));
        assert_eq!(keys("new-child", cx), Some(key_label("shift-n")));
        assert_eq!(keys("move-down", cx), Some(key_label("shift-down")));
        assert_eq!(keys("outdent", cx), Some(key_label("shift-left")));
        // Start Chat is the Outline's Mod+Enter.
        assert_eq!(keys("start-chat", cx), Some(key_label("mod-enter")));
        // Unbound commands and helper entries show no key.
        assert_eq!(keys("move-to", cx), None);
        assert_eq!(keys("copy-path", cx), None);
        assert!(cx.debug_bounds("outline-menu-key-move-to").is_none());
        assert!(cx.debug_bounds("outline-menu-key-rename").is_some());

        cx.update(|_, cx| {
            settings::update(SavePolicy::Debounced, cx, |s| {
                s.keymap.set(ShortcutId::MoveWorkItem, "mod-alt-k".into())
            });
        });
        redraw(cx);
        assert_eq!(keys("move-to", cx), Some(key_label("mod-alt-k")));
        assert!(cx.debug_bounds("outline-menu-key-move-to").is_some());

        cx.update(|_, cx| {
            settings::update(SavePolicy::Debounced, cx, |s| {
                s.keymap.set(ShortcutId::MoveWorkItem, String::new())
            });
        });
        redraw(cx);
        assert_eq!(keys("move-to", cx), None);
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
        assert_eq!(active(&shell, cx), Some("recent-changes"));
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
        assert!(
            outline_focused(&outline, cx),
            "a click keeps focus in the tree"
        );

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
            assert_eq!(
                moves[0]["destinationParentWorkItemId"],
                serde_json::Value::Null
            );
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

    /// The Chat-row moves through the real keymap: a left click on a Chat
    /// row, then Shift+Down / Shift+Left / Shift+Up re-associate it (one
    /// `assignChatToWorkItem` / `removeChatFromWorkItem` each, shown at once,
    /// the selection riding along); Shift+Right does nothing; the menu's
    /// Move to Next Item sends the same request as Shift+Down.
    #[gpui::test]
    fn click_then_shift_arrows_move_a_chat_row_in_the_shell(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let snapshot = crate::work_outline::store::fixture_snapshot("s1");
        let (shell, outline, calls, cx) = menu_shell(cx, dir.path(), snapshot);
        let chat = row_key::chat(CHAT_UI);
        let at = cx
            .debug_bounds(selector(format!("outline-row-{chat}")))
            .expect("the Chat row is shown")
            .center();
        cx.simulate_click(at, gpui::Modifiers::default());
        cx.run_until_parked();
        redraw(cx);
        assert_eq!(selected(&outline, cx), Some(chat.clone()));
        assert!(outline_focused(&outline, cx));
        let parent = |cx: &mut gpui::VisualTestContext| {
            outline.read_with(cx, |outline, _| {
                let model = outline.model();
                let ix = model.position(&row_key::chat(CHAT_UI)).unwrap();
                model.rows[ix].parent.map(|p| model.rows[p].key.clone())
            })
        };
        let call = |ix: usize| calls.lock().unwrap()[ix].clone();

        press(cx, "shift-down");
        assert_eq!(call(0)["op"], "assignChatToWorkItem");
        assert_eq!(call(0)["chatId"], CHAT_UI);
        assert_eq!(call(0)["workItemId"], HOUSEKEEPING);
        assert_eq!(parent(cx), Some(row_key::item("s1", HOUSEKEEPING)));
        assert_eq!(selected(&outline, cx), Some(chat.clone()));

        press(cx, "shift-left");
        assert_eq!(call(1)["op"], "removeChatFromWorkItem");
        assert_eq!(call(1)["chatId"], CHAT_UI);
        assert_eq!(parent(cx), Some(row_key::unassociated(Some("s1"))));

        press(cx, "shift-right");
        assert_eq!(calls.lock().unwrap().len(), 2, "Shift+Right does nothing");

        press(cx, "shift-up");
        assert_eq!(call(2)["workItemId"], HOUSEKEEPING);
        assert_eq!(selected(&outline, cx), Some(chat.clone()));

        // The menu entry runs the same move as its key.
        right_click(cx, &row_key::chat(CHAT_API));
        assert!(menu_open(&shell, cx));
        click(cx, "move-to-next-item");
        assert!(!menu_open(&shell, cx));
        assert_eq!(call(3)["op"], "assignChatToWorkItem");
        assert_eq!(call(3)["chatId"], CHAT_API);
        assert_eq!(call(3)["workItemId"], API_AUTH);
        assert_eq!(selected(&outline, cx), Some(row_key::chat(CHAT_API)));
    }

    fn click_row(cx: &mut gpui::VisualTestContext, key: &str) {
        let row = cx
            .debug_bounds(selector(format!("outline-row-{key}")))
            .unwrap_or_else(|| panic!("row {key} is shown"));
        cx.simulate_click(row.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        redraw(cx);
    }

    /// Archive session as the app binds it on this platform.
    fn archive_key() -> String {
        platform_combo(ShortcutId::ArchiveSession.default_combo())
    }

    fn archived_chats(shell: &Entity<Shell>, cx: &mut gpui::VisualTestContext) -> Vec<String> {
        shell.read_with(cx, |shell, _| {
            shell
                .mutations
                .iter()
                .filter(|m| m["op"] == "setChatArchived" && m["archived"] == true)
                .filter_map(|m| m["chatId"].as_str().map(str::to_owned))
                .collect()
        })
    }

    /// Shift+Cmd+A in the Outline tree, through the app keymap: a Chat row
    /// is archived (the sidebar's path) without opening it and the
    /// selection moves on; a work item opens its Archive dialog with the
    /// descendant choice; the group row ignores it.
    #[gpui::test]
    fn archive_key_acts_on_the_selected_outline_row(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let fixture = crate::work_outline::store::fixture_snapshot("s1");
        let (shell, outline, _calls, cx) = menu_shell(cx, dir.path(), fixture);

        let chat = row_key::chat(CHAT_UI);
        click_row(cx, &chat);
        assert!(outline_focused(&outline, cx));
        press(cx, &archive_key());
        assert_eq!(archived_chats(&shell, cx), [CHAT_UI]);
        let after = selected(&outline, cx);
        assert!(
            after.is_some() && after.as_deref() != Some(chat.as_str()),
            "{after:?}"
        );
        shell.read_with(cx, |shell, cx| {
            assert!(
                matches!(shell.route, Route::Overview),
                "the Chat never opens"
            );
            assert_eq!(shell.state.read(cx).selected_chat, None);
            assert!(shell.work_outline_modal.is_none());
        });

        // A work item: its Archive dialog, nothing archived yet.
        click_row(cx, &row_key::item("s1", API));
        press(cx, &archive_key());
        shell.read_with(cx, |shell, _| {
            assert!(
                matches!(
                    shell.work_outline_modal,
                    Some(crate::shell::work_outline_ui::WorkOutlineModal::Archive(_))
                ),
                "the Archive dialog opens"
            );
        });
        assert_eq!(archived_chats(&shell, cx), [CHAT_UI]);
        press(cx, "escape");

        // The Unassociated group: nothing.
        let mutations = shell.read_with(cx, |shell, _| shell.mutations.len());
        click_row(cx, &row_key::unassociated(Some("s1")));
        press(cx, &archive_key());
        shell.read_with(cx, |shell, _| {
            assert_eq!(shell.mutations.len(), mutations);
            assert!(shell.work_outline_modal.is_none());
        });
    }

    /// Outside the Outline the shortcut still archives the open Chat.
    #[gpui::test]
    fn archive_key_keeps_archiving_the_open_chat_outside_the_outline(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let fixture = crate::work_outline::store::fixture_snapshot("s1");
        let (shell, _outline, _calls, cx) = menu_shell(cx, dir.path(), fixture);
        shell.update(cx, |shell, cx| shell.open_chat(CHAT_API.into(), cx));
        redraw(cx);
        press(cx, &archive_key());
        assert_eq!(archived_chats(&shell, cx), [CHAT_API]);
    }

    /// The Archive Chat and Archive… entries show the shortcut's live key.
    #[gpui::test]
    fn archive_entries_show_the_archive_shortcut(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let fixture = crate::work_outline::store::fixture_snapshot("s1");
        let (shell, _outline, _calls, cx) = menu_shell(cx, dir.path(), fixture);
        let archive = Some(key_label(ShortcutId::ArchiveSession.default_combo()));
        right_click(cx, &row_key::chat(CHAT_UI));
        assert_eq!(row(&shell, cx, "archive-chat").keys, archive);
        press(cx, "escape");
        settle(&shell, cx);
        right_click(cx, &row_key::item("s1", API));
        assert_eq!(row(&shell, cx, "archive").keys, archive);
        cx.update(|_, cx| {
            settings::update(SavePolicy::Debounced, cx, |s| {
                s.keymap.set(ShortcutId::ArchiveSession, "mod-alt-x".into())
            });
        });
        redraw(cx);
        assert_eq!(
            row(&shell, cx, "archive").keys,
            Some(key_label("mod-alt-x"))
        );
    }

    fn start_chat_item(shell: &Entity<Shell>, cx: &mut gpui::VisualTestContext) -> Option<String> {
        shell.read_with(cx, |shell, _| match &shell.work_outline_modal {
            Some(crate::shell::work_outline_ui::WorkOutlineModal::StartChat(dialog)) => {
                Some(dialog.item_id.clone())
            }
            _ => None,
        })
    }

    /// Mod+Enter in the Outline tree, through the app keymap: Start Chat in
    /// the selected work item, or in a Chat row's work item; a notice for an
    /// unassociated Chat. Enter still opens. In the composer Mod+Enter is
    /// never Start Chat.
    #[gpui::test]
    fn mod_enter_starts_a_chat_in_the_selected_rows_work_item(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let fixture = crate::work_outline::store::fixture_snapshot("s1");
        let (shell, outline, _calls, cx) = menu_shell(cx, dir.path(), fixture);
        let start = platform_combo("mod-enter");

        click_row(cx, &row_key::item("s1", HOUSEKEEPING));
        press(cx, &start);
        assert_eq!(
            start_chat_item(&shell, cx).as_deref(),
            Some(HOUSEKEEPING),
            "a work item row"
        );
        press(cx, "escape");
        assert_eq!(start_chat_item(&shell, cx), None);

        click_row(cx, &row_key::chat(CHAT_UI));
        press(cx, &start);
        assert_eq!(
            start_chat_item(&shell, cx).as_deref(),
            Some(UI),
            "a Chat row starts in its work item"
        );
        press(cx, "escape");

        click_row(cx, &row_key::chat(CHAT_LOOSE_A));
        press(cx, &start);
        assert_eq!(start_chat_item(&shell, cx), None);
        assert_eq!(
            outline.read_with(cx, |outline, _| outline.notice().map(str::to_owned)),
            Some("This Chat is not in a work item".to_owned())
        );

        // The composer keeps Mod+Enter for itself.
        shell.update(cx, |shell, cx| shell.open_chat(CHAT_UI.into(), cx));
        redraw(cx);
        press(cx, &start);
        assert_eq!(start_chat_item(&shell, cx), None);
    }

    /// Every entry of every row kind's menu (both pages) shows the live key
    /// of its action whenever one is bound: with every Work Outline command
    /// bound, no keyed entry may come up blank, and every shown key is the
    /// one its action's binding resolves to.
    #[gpui::test]
    fn every_menu_entry_with_a_bound_action_shows_its_key(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let fixture = crate::work_outline::instructions::fixture::snapshot("s1");
        let (shell, _outline, _calls, cx) = menu_shell(cx, dir.path(), fixture);
        cx.update(|_, cx| {
            settings::update(SavePolicy::Debounced, cx, |s| {
                for (ix, command) in WorkOutlineCommand::ALL.into_iter().enumerate() {
                    let key = (b'a' + ix as u8) as char;
                    s.keymap
                        .set(command.shortcut_id(), format!("mod-alt-shift-{key}"));
                }
            });
        });
        let audit = |shell: &Entity<Shell>, cx: &mut gpui::VisualTestContext, what: &str| {
            let mut seen = 0;
            for page in [MenuPage::Root, MenuPage::Status] {
                shell.update(cx, |shell, _| {
                    if let Some(state) = shell.work_outline_menu.open_mut() {
                        state.page = page;
                    }
                });
                shell.read_with(cx, |shell, cx| {
                    let keymap = settings::current(cx).keymap;
                    let state = shell.work_outline_menu.get().expect("menu open");
                    let rows = shell.work_outline_menu_rows(cx);
                    for (entry, row) in state.entries().iter().zip(rows) {
                        let hint = entry.action.hint();
                        assert_eq!(entry.hint, hint, "{what}: {}", entry.id);
                        let bound = hints::hint_keys(&keymap, hint);
                        assert_eq!(row.keys, bound, "{what}: {}", entry.id);
                        if hint != HintKey::None {
                            assert!(row.keys.is_some(), "{what}: {} shows no key", entry.id);
                            seen += 1;
                        }
                    }
                });
            }
            assert!(seen > 0, "{what}");
        };
        let menus = [
            ("work item", row_key::item("s1", API)),
            ("chat", row_key::chat(CHAT_API)),
            ("unassociated chat", row_key::chat(CHAT_LOOSE_A)),
        ];
        for (what, key) in menus {
            right_click(cx, &key);
            audit(&shell, cx, what);
            press(cx, "escape");
            settle(&shell, cx);
        }
        let archive = key_label(ShortcutId::ArchiveSession.default_combo());
        right_click(cx, &row_key::chat(CHAT_API));
        assert_eq!(row(&shell, cx, "archive-chat").keys, Some(archive.clone()));
        press(cx, "escape");
        settle(&shell, cx);
        right_click(cx, &row_key::item("s1", API));
        assert_eq!(row(&shell, cx, "archive").keys, Some(archive));
        press(cx, "escape");
        settle(&shell, cx);

        // The Space root entry and the Unassociated group.
        let entry = cx.debug_bounds("outline-space-root-entry").unwrap();
        cx.simulate_mouse_down(
            entry.center(),
            MouseButton::Right,
            gpui::Modifiers::default(),
        );
        cx.run_until_parked();
        redraw(cx);
        audit(&shell, cx, "space root");
        press(cx, "escape");
        settle(&shell, cx);
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                state.selected_chat = Some(CHAT_LOOSE_A.into())
            })
        });
        right_click(cx, &row_key::unassociated(Some("s1")));
        audit(&shell, cx, "unassociated group");
    }

    /// [`menu_shell`] plus a second Space `s2` ("beta") with one
    /// unassociated Chat, so the Outline shows Space root rows.
    fn two_space_shell<'a>(
        cx: &'a mut gpui::TestAppContext,
        dir: &std::path::Path,
    ) -> (
        Entity<Shell>,
        Entity<OutlineView>,
        Calls,
        &'a mut gpui::VisualTestContext,
    ) {
        let fixture = crate::work_outline::store::fixture_snapshot("s1");
        let (shell, outline, calls, cx) = menu_shell(cx, dir, fixture);
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, cx| {
                state.spaces.push(
                    serde_json::from_value(serde_json::json!({
                        "id": "s2", "deviceId": "d", "path": "/work/beta",
                        "createdAt": "2026-01-01T00:00:00Z"
                    }))
                    .unwrap(),
                );
                state.chats.push(
                    serde_json::from_value(serde_json::json!({
                        "id": "chat-beta", "deviceId": "d", "archived": false,
                        "createdAt": "2026-01-01T00:00:00Z", "spaceId": "s2",
                        "title": "Chat beta",
                    }))
                    .unwrap(),
                );
                let mut s2 = crate::work_outline::store::fixture_snapshot("s2");
                for item in &mut s2.items {
                    item.chat_ids.clear();
                }
                s2.assignments.clear();
                s2.unassociated_chat_ids = vec!["chat-beta".into()];
                state.set_work_outline_visible(vec!["s1".into(), "s2".into()]);
                assert!(state.apply_work_outline_frame("s2", s2));
                cx.notify();
            })
        });
        redraw(cx);
        (shell, outline, calls, cx)
    }

    fn space_roots(outline: &Entity<OutlineView>, cx: &mut gpui::VisualTestContext) -> Vec<String> {
        outline.read_with(cx, |outline, _| {
            outline
                .model()
                .rows
                .iter()
                .filter_map(|row| match &row.kind {
                    crate::work_outline::model::OutlineRowKind::Space { space_id, .. } => {
                        Some(space_id.clone())
                    }
                    _ => None,
                })
                .collect()
        })
    }

    /// A Space row's menu: Move Up / Down (with their keys, greyed at the
    /// ends), Hide from Outline, and the sidebar's Rename and Remove flows.
    /// Delete opens the existing confirmation; Cancel deletes nothing.
    #[gpui::test]
    fn space_row_menu_reorders_hides_renames_and_confirms_delete(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, outline, calls, cx) = two_space_shell(cx, dir.path());
        assert_eq!(space_roots(&outline, cx), ["s1", "s2"]);
        let beta = row_key::space("s2");

        right_click(cx, &beta);
        assert_eq!(selected(&outline, cx), Some(beta.clone()));
        assert_eq!(
            ids(&shell, cx),
            [
                "new-item",
                "space-instructions",
                "execution",
                "move-up",
                "move-down",
                "hide-space",
                "rename-space",
                "claude-account",
                "codex-account",
                "required-actions",
                "undo",
                "redo",
                "recent-changes",
                "shortcuts",
                "delete-space",
            ]
        );
        assert_eq!(row(&shell, cx, "move-up").keys, Some(key_label("shift-up")));
        assert_eq!(row(&shell, cx, "move-up").disabled, None);
        assert_eq!(
            row(&shell, cx, "move-down").disabled.as_deref(),
            Some("Already the last project")
        );
        // Move Up dispatches the ⇧↑ action on the selected Space row.
        click(cx, "move-up");
        assert_eq!(space_roots(&outline, cx), ["s2", "s1"]);
        settle(&shell, cx);

        // Rename: the sidebar's dialog for this Space.
        right_click(cx, &beta);
        click(cx, "rename-space");
        assert_eq!(
            shell.read_with(cx, |shell, _| shell
                .rename_space_dialog
                .as_ref()
                .map(|d| d.space_id.clone())),
            Some("s2".to_string())
        );
        shell.update(cx, |shell, cx| {
            shell.rename_space_dialog = None;
            cx.notify();
        });
        settle(&shell, cx);

        // Delete: the existing destructive confirmation, with the copy that
        // names the index and the Work Outline and recommends Hide.
        right_click(cx, &beta);
        click(cx, "delete-space");
        assert_eq!(
            shell.read_with(cx, |shell, _| shell.delete_space_confirm.clone()),
            Some("s2".to_string())
        );
        assert!(cx.debug_bounds("delete-space-confirm").is_some());
        let cancel = cx
            .debug_bounds("delete-space-cancel")
            .expect("Cancel renders");
        cx.simulate_click(cancel.center(), gpui::Modifiers::default());
        redraw(cx);
        assert_eq!(
            shell.read_with(cx, |shell, _| shell.delete_space_confirm.clone()),
            None
        );
        shell.read_with(cx, |shell, cx| {
            let state = shell.state.read(cx);
            assert!(state.spaces.iter().any(|space| space.id == "s2"));
            assert!(state.chats.iter().any(|chat| chat.id == "chat-beta"));
        });
        assert!(calls.lock().unwrap().is_empty(), "nothing sent");
        settle(&shell, cx);

        // Hide from Outline: gone from the Outline, still in the app.
        let sidebar = shell.update(cx, |shell, cx| shell.sidebar_visible_order(cx));
        assert!(sidebar.iter().any(|id| id == "chat-beta"), "{sidebar:?}");
        right_click(cx, &beta);
        click(cx, "hide-space");
        assert_eq!(space_roots(&outline, cx), ["s1"]);
        assert!(outline.read_with(cx, |o, _| {
            o.model().row(&row_key::chat("chat-beta")).is_none()
        }));
        assert!(cx.debug_bounds("outline-hidden-spaces").is_some());
        assert_eq!(
            shell.update(cx, |shell, cx| shell.sidebar_visible_order(cx)),
            sidebar,
            "the sidebar is unaffected"
        );
        shell.read_with(cx, |shell, cx| {
            assert!(
                shell
                    .state
                    .read(cx)
                    .chats
                    .iter()
                    .any(|chat| chat.id == "chat-beta")
            );
        });
    }
    type AccountCalls = Arc<Mutex<Vec<(String, serde_json::Value)>>>;

    fn accounts_snapshot() -> mast_proto::ManagedAccountsSnapshot {
        serde_json::from_value(serde_json::json!({
            "accounts": [
                { "id": "a1", "harness": "claude-code", "label": "Work", "signedIn": true,
                  "email": "w@example.com" },
                { "id": "a2", "harness": "codex", "label": "Side", "signedIn": false }
            ],
            "projectDefaults": [],
            "chatPins": [
                { "chatId": "chat-api", "harness": "claude-code", "accountId": "a1" },
                { "chatId": "chat-ui", "harness": "claude-code" }
            ],
            "saved": [
                { "id": "slotsaved0000001", "harness": "claude-code",
                  "email": "saved@example.com", "planLabel": "Max" },
                { "id": "slotlive00000001", "harness": "claude-code",
                  "email": "live@example.com", "active": true },
                { "id": "slotcodex0000001", "harness": "codex", "email": "cx@example.com" }
            ]
        }))
        .unwrap()
    }

    fn script_accounts(shell: &Entity<Shell>, cx: &mut gpui::VisualTestContext) -> AccountCalls {
        let calls: AccountCalls = Arc::default();
        let sink = calls.clone();
        let snapshot = accounts_snapshot();
        let reply = serde_json::to_value(&snapshot).unwrap();
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                state.managed_accounts.insert("d".into(), snapshot);
                state.accounts_rpc_override = Some(Arc::new(move |method, params| {
                    sink.lock().unwrap().push((method.to_string(), params));
                    Ok(reply.clone())
                }));
            })
        });
        calls
    }

    /// The Space menu lists a per-harness account entry; its picker lists
    /// Default plus only that harness's accounts and Selecting one sends
    /// SetProjectAccount for the project.
    #[gpui::test]
    fn project_account_picker_lists_harness_accounts_and_sets_project(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, _outline, _calls, cx) = two_space_shell(cx, dir.path());
        let accounts = script_accounts(&shell, cx);
        let beta = row_key::space("s2");

        right_click(cx, &beta);
        assert_eq!(
            row(&shell, cx, "claude-account").label,
            "Claude account: Default\u{2026}"
        );
        click(cx, "claude-account");
        assert!(cx.debug_bounds("account-pick-default").is_some());
        assert!(cx.debug_bounds("account-pick-a1").is_some());
        assert!(
            cx.debug_bounds("account-pick-a2").is_none(),
            "Codex accounts are not offered for Claude"
        );
        assert!(cx.debug_bounds("account-pick-add").is_some());

        let work = cx.debug_bounds("account-pick-a1").unwrap();
        cx.simulate_click(work.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        redraw(cx);
        let set: Vec<_> = accounts
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _)| m == mast_rpc::methods::SET_PROJECT_ACCOUNT)
            .cloned()
            .collect();
        assert_eq!(set.len(), 1, "{set:?}");
        assert_eq!(
            set[0].1,
            serde_json::json!({
                "spaceId": "s2", "harness": "claude-code", "accountId": "a1",
                "targetDeviceId": "d"
            })
        );
        assert!(
            cx.debug_bounds("account-pick-default").is_none(),
            "picker closes after choosing"
        );
    }

    /// The sign-in sheet follows the CLI's output (URL, code prompt) and a
    /// pasted code goes out as one SubmitManagedAccountLoginCode call, with the
    /// field cleared afterwards.
    #[gpui::test]
    fn sign_in_sheet_submits_the_pasted_code(cx: &mut gpui::TestAppContext) {
        use crate::managed_accounts::AccountFlow;
        let dir = tempfile::tempdir().unwrap();
        let (shell, _outline, _calls, cx) = two_space_shell(cx, dir.path());
        let accounts = script_accounts(&shell, cx);
        let state = shell.read_with(cx, |shell, _| shell.state.clone());
        let flow = cx.new(|cx| {
            AccountFlow::new(
                state,
                mast_proto::HarnessId::ClaudeCode,
                None,
                Some("d".into()),
                cx,
            )
        });
        let poll = mast_proto::AgentLoginPoll {
            status: mast_proto::AgentLoginStatus::Pending,
            message: None,
            url: Some("https://claude.example/a".into()),
            awaiting_code: true,
            device_code: None,
        };
        flow.update(cx, |flow, cx| {
            flow.waiting_for_test("login-1");
            flow.apply_poll(&poll, cx);
        });
        flow.read_with(cx, |flow, _| {
            let view = flow.login_view();
            assert_eq!(view.url.as_deref(), Some("https://claude.example/a"));
            assert!(view.awaiting_code && view.opened);
        });
        // Empty input submits nothing.
        flow.update(cx, |flow, cx| flow.submit_code(cx));
        cx.run_until_parked();
        let submitted = |accounts: &AccountCalls| {
            accounts
                .lock()
                .unwrap()
                .iter()
                .filter(|(m, _)| m == mast_rpc::methods::SUBMIT_MANAGED_ACCOUNT_LOGIN_CODE)
                .cloned()
                .collect::<Vec<_>>()
        };
        assert!(submitted(&accounts).is_empty());

        flow.update(cx, |flow, cx| {
            flow.set_code_for_test("abc-123", cx);
            flow.submit_code(cx);
        });
        cx.run_until_parked();
        let calls = submitted(&accounts);
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(calls[0].1["loginId"], "login-1");
        assert_eq!(calls[0].1["code"], "abc-123");
        flow.read_with(cx, |flow, cx| {
            assert_eq!(flow.code_text_for_test(cx), "");
            assert!(!flow.login_view().submitting);
        });
    }

    /// Saved Settings logins show in the picker for their harness; the live
    /// Default one is disabled, and choosing another promotes it (one RPC).
    #[gpui::test]
    fn project_account_picker_promotes_a_saved_login(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, _outline, _calls, cx) = two_space_shell(cx, dir.path());
        let accounts = script_accounts(&shell, cx);
        right_click(cx, &row_key::space("s2"));
        click(cx, "claude-account");
        assert!(
            cx.debug_bounds("account-pick-saved-slotsaved0000001")
                .is_some()
        );
        assert!(
            cx.debug_bounds("account-pick-saved-slotlive00000001")
                .is_some()
        );
        assert!(
            cx.debug_bounds("account-pick-saved-slotcodex0000001")
                .is_none(),
            "Codex saved logins are not offered for Claude"
        );

        let live = cx
            .debug_bounds("account-pick-saved-slotlive00000001")
            .unwrap();
        cx.simulate_click(live.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert!(
            !accounts
                .lock()
                .unwrap()
                .iter()
                .any(|(m, _)| m == mast_rpc::methods::PROMOTE_SAVED_ACCOUNT),
            "the live Default login is not selectable"
        );

        let saved = cx
            .debug_bounds("account-pick-saved-slotsaved0000001")
            .unwrap();
        cx.simulate_click(saved.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        redraw(cx);
        let promote: Vec<_> = accounts
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _)| m == mast_rpc::methods::PROMOTE_SAVED_ACCOUNT)
            .cloned()
            .collect();
        assert_eq!(promote.len(), 1, "{promote:?}");
        assert_eq!(
            promote[0].1,
            serde_json::json!({
                "spaceId": "s2", "harness": "claude-code", "slotId": "slotsaved0000001",
                "targetDeviceId": "d"
            })
        );
        assert!(cx.debug_bounds("account-pick-default").is_none());
    }

    /// The chat account pill shows only for a chat pinned to a managed
    /// account; Default and unpinned chats stay quiet.
    #[gpui::test]
    fn chat_account_badge_state_only_for_a_pinned_managed_account(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, _outline, _calls, cx) = two_space_shell(cx, dir.path());
        let _ = script_accounts(&shell, cx);
        redraw(cx);
        shell.read_with(cx, |shell, cx| {
            let state = shell.state.read(cx);
            assert_eq!(
                state.chat_account_badge("chat-api").as_deref(),
                Some("Work")
            );
            assert_eq!(state.chat_account_badge("chat-ui"), None);
            assert_eq!(state.chat_account_badge("chat-auth"), None);
        });
    }
}
