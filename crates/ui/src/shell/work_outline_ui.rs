//! Shell-owned Work Outline surfaces: the confirmation modals (discard an
//! unsaved draft, archive with an explicit descendant choice), the external
//! reference opener, the command router, and the keymap bindings.
//!
//! The Overview never owns a window modal; it calls through its
//! `WeakEntity<Shell>`. Decisions are delivered through the request's
//! `on_decision` closure, deferred to after the current Shell update so a
//! callback can update any entity without re-entering the Shell.
//!
//! Escape: `Shell::on_key_down_capture` cancels the modal (reporting a
//! cancelled decision); `overlay_owns_keyboard` and `capture_escape_surface`
//! also know about it, like the other shell dialogs.
use super::*;

use crate::markdown::links::LinkTarget;
use crate::work_outline::{
    WorkOutlineArchiveRequest, WorkOutlineCommand, WorkOutlineConfirmRequest,
    WorkOutlineParentChoice,
};
use zeron_proto::DescendantArchivePolicy;

/// Fire one Work Outline command from a keymap binding. One action carrying
/// the command, like `JumpSession`, rather than nine action types.
#[derive(Clone, PartialEq, Action)]
#[action(namespace = shell, no_json)]
pub struct RunWorkOutlineCommand(pub WorkOutlineCommand);

type ConfirmCallback = Box<dyn FnOnce(bool, &mut Window, &mut App) + 'static>;
type ArchiveCallback =
    Box<dyn FnOnce(Option<DescendantArchivePolicy>, &mut Window, &mut App) + 'static>;

pub(super) struct ConfirmDialog {
    title: SharedString,
    body: SharedString,
    confirm_label: SharedString,
    cancel_label: SharedString,
    destructive: bool,
    on_decision: Option<ConfirmCallback>,
}

/// The explicit archive choice. There is no default: the confirm button stays
/// disabled until the user picks one (unless there are no descendants).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ArchiveChoice {
    ArchiveDescendants,
    MoveDescendants,
}

pub(super) struct ArchiveDialog {
    item_title: SharedString,
    descendant_count: usize,
    parents: Vec<WorkOutlineParentChoice>,
    choice: Option<ArchiveChoice>,
    parent_ix: usize,
    on_decision: Option<ArchiveCallback>,
}

impl ArchiveDialog {
    fn new(request: WorkOutlineArchiveRequest) -> Self {
        Self {
            choice: (request.descendant_count == 0).then_some(ArchiveChoice::ArchiveDescendants),
            item_title: request.item_title,
            descendant_count: request.descendant_count,
            parents: request.parents,
            parent_ix: 0,
            on_decision: Some(request.on_decision),
        }
    }

    /// The policy the current selection means; `None` while no valid choice
    /// has been made (confirm disabled).
    pub(super) fn policy(&self) -> Option<DescendantArchivePolicy> {
        match self.choice? {
            ArchiveChoice::ArchiveDescendants => Some(DescendantArchivePolicy::ArchiveDescendants),
            ArchiveChoice::MoveDescendants => {
                let parent = self.parents.get(self.parent_ix)?;
                Some(DescendantArchivePolicy::MoveDescendants {
                    destination_parent_work_item_id: parent.work_item_id.clone(),
                })
            }
        }
    }
}

pub(super) enum WorkOutlineModal {
    Confirm(ConfirmDialog),
    Archive(ArchiveDialog),
}

/// How a work item reference link should open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ReferenceOpen {
    /// The internal browser surface of the selected chat.
    Internal(String),
    External(String),
    Rejected,
}

/// Pure routing for [`Shell::open_work_item_reference`]. Only http(s) URLs
/// pass (`LinkTarget` validation). The internal browser exists only on the
/// Chat route with a chat selected and the web-link preference on; from
/// Overview-hosted details there is no source chat, so the external browser
/// is used (m0-contract section 7).
pub(super) fn reference_open(
    url: &str,
    on_chat_route_with_chat: bool,
    open_web_links_in_zeron: bool,
    embedded_platform: bool,
) -> ReferenceOpen {
    match LinkTarget::new(url, url).navigation {
        Err(_) => ReferenceOpen::Rejected,
        Ok(navigation) => {
            if on_chat_route_with_chat && open_web_links_in_zeron && embedded_platform {
                ReferenceOpen::Internal(navigation)
            } else {
                ReferenceOpen::External(navigation)
            }
        }
    }
}

/// Key contexts that hold a text input. Outline navigation keys must never
/// fire while one of these owns focus.
pub(crate) const TEXT_INPUT_CONTEXTS: [&str; 3] = ["Composer", "MessageComposer", "PaletteSearch"];

/// The context predicate every bare-key outline binding uses.
pub(crate) const OUTLINE_NAVIGATION_CONTEXT: &str =
    "WorkOutline && !Composer && !MessageComposer && !PaletteSearch";

/// Outline navigation, one entry per action defined in
/// `work_outline::outline_view`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutlineNavKey {
    SelectParent,
    SelectChild,
    SelectNextSibling,
    SelectPreviousSibling,
    Expand,
    Collapse,
    OpenSelected,
    RenameSelected,
    CreateSibling,
    CreateChild,
}

/// Default outline keys (identical on every platform: no primary modifier is
/// involved, and Alt is Option on macOS).
///
/// - Up / Down: previous / next sibling
/// - Alt+Up / Alt+Down: parent / first child
/// - Left / Right: collapse / expand
/// - Enter: open the selected item (Work Item Details)
/// - F2: rename in place
/// - N / Shift+N: create a sibling / child
pub(crate) const OUTLINE_NAVIGATION_KEYS: [(&str, OutlineNavKey); 10] = [
    ("up", OutlineNavKey::SelectPreviousSibling),
    ("down", OutlineNavKey::SelectNextSibling),
    ("alt-up", OutlineNavKey::SelectParent),
    ("alt-down", OutlineNavKey::SelectChild),
    ("left", OutlineNavKey::Collapse),
    ("right", OutlineNavKey::Expand),
    ("enter", OutlineNavKey::OpenSelected),
    ("f2", OutlineNavKey::RenameSelected),
    ("n", OutlineNavKey::CreateSibling),
    ("shift-n", OutlineNavKey::CreateChild),
];

/// Bindings for the configurable Work Outline commands. An empty combo is
/// unbound (the user cleared it, or the command ships without a default); an
/// unparseable one falls back to the default, or to unbound when there is
/// none.
pub(crate) fn work_outline_bindings(keymap: &KeymapConfig) -> Vec<KeyBinding> {
    WorkOutlineCommand::ALL
        .into_iter()
        .filter_map(|command| {
            let id = command.shortcut_id();
            let combo = keymap.get(id);
            if combo.is_empty() {
                return None;
            }
            let candidate = platform_combo(combo);
            let resolved = if Keystroke::parse(&candidate).is_ok() {
                candidate
            } else {
                let fallback = id.default_combo();
                if fallback.is_empty() {
                    return None;
                }
                platform_combo(fallback)
            };
            Some(KeyBinding::new(
                &resolved,
                RunWorkOutlineCommand(command),
                None,
            ))
        })
        .collect()
}

/// Binds [`OUTLINE_NAVIGATION_KEYS`] under [`OUTLINE_NAVIGATION_CONTEXT`] to
/// the actions of `work_outline::outline_view`.
pub(crate) fn bind_work_outline_navigation(cx: &mut App) {
    use crate::work_outline::outline_view::{
        Collapse, CreateChild, CreateSibling, Expand, OpenSelected, RenameSelected, SelectChild,
        SelectNextSibling, SelectParent, SelectPreviousSibling,
    };
    let context = Some(OUTLINE_NAVIGATION_CONTEXT);
    cx.bind_keys(OUTLINE_NAVIGATION_KEYS.iter().map(|(keys, key)| match key {
        OutlineNavKey::SelectParent => KeyBinding::new(keys, SelectParent, context),
        OutlineNavKey::SelectChild => KeyBinding::new(keys, SelectChild, context),
        OutlineNavKey::SelectNextSibling => KeyBinding::new(keys, SelectNextSibling, context),
        OutlineNavKey::SelectPreviousSibling => {
            KeyBinding::new(keys, SelectPreviousSibling, context)
        }
        OutlineNavKey::Expand => KeyBinding::new(keys, Expand, context),
        OutlineNavKey::Collapse => KeyBinding::new(keys, Collapse, context),
        OutlineNavKey::OpenSelected => KeyBinding::new(keys, OpenSelected, context),
        OutlineNavKey::RenameSelected => KeyBinding::new(keys, RenameSelected, context),
        OutlineNavKey::CreateSibling => KeyBinding::new(keys, CreateSibling, context),
        OutlineNavKey::CreateChild => KeyBinding::new(keys, CreateChild, context),
    }));
}

/// Run the modal's decision callback (at most once).
fn deliver_decision(modal: WorkOutlineModal, accept: bool, window: &mut Window, cx: &mut App) {
    match modal {
        WorkOutlineModal::Confirm(mut dialog) => {
            if let Some(callback) = dialog.on_decision.take() {
                callback(accept, window, cx);
            }
        }
        WorkOutlineModal::Archive(mut dialog) => {
            let policy = if accept { dialog.policy() } else { None };
            if let Some(callback) = dialog.on_decision.take() {
                callback(policy, window, cx);
            }
        }
    }
}

impl Shell {
    /// Whether a Work Outline modal is open.
    pub(super) fn work_outline_modal_open(&self) -> bool {
        self.work_outline_modal.is_some()
    }

    /// Open the external reference of a work item per the web-link
    /// preference. See [`reference_open`]. Never touches manual PR/ticket
    /// links: it only opens a URL.
    pub(crate) fn open_work_item_reference(
        &mut self,
        url: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let on_chat = matches!(self.route, Route::Chat) && !self.active_chat.is_empty();
        let in_app = crate::settings::current(cx).open_web_links_in_zeron;
        match reference_open(
            url,
            on_chat,
            in_app,
            cfg!(any(target_os = "macos", target_os = "linux")),
        ) {
            ReferenceOpen::Internal(navigation) => {
                self.set_surfaces_open(true, cx);
                self.add_browser_surface(Some(navigation), window, cx);
            }
            ReferenceOpen::External(navigation) => cx.open_url(&navigation),
            ReferenceOpen::Rejected => {
                self.sidebar_notice = Some("That reference link can\u{2019}t be opened".into());
                cx.notify();
            }
        }
    }

    /// Show a window modal (discard draft, move with descendants, ...) and
    /// deliver the decision through `request.on_decision` (`true` =
    /// confirmed). Any other way of closing reports `false`.
    pub(crate) fn open_work_outline_confirm(
        &mut self,
        request: WorkOutlineConfirmRequest,
        cx: &mut Context<Self>,
    ) {
        self.begin_work_outline_modal(
            WorkOutlineModal::Confirm(ConfirmDialog {
                title: request.title,
                body: request.body,
                confirm_label: request.confirm_label,
                cancel_label: request.cancel_label,
                destructive: request.destructive,
                on_decision: Some(request.on_decision),
            }),
            cx,
        );
    }

    /// Show the archive dialog with its explicit descendant choice.
    pub(crate) fn open_work_outline_archive_confirm(
        &mut self,
        request: WorkOutlineArchiveRequest,
        cx: &mut Context<Self>,
    ) {
        self.begin_work_outline_modal(WorkOutlineModal::Archive(ArchiveDialog::new(request)), cx);
    }

    fn begin_work_outline_modal(&mut self, modal: WorkOutlineModal, cx: &mut Context<Self>) {
        // A newer request supersedes an open one, which reports a cancel. No
        // window is at hand here, so the report is delivered on the next
        // render (`render_work_outline_modal`).
        if let Some(previous) = self.work_outline_modal.take() {
            self.work_outline_superseded.push(previous);
        }
        self.command_palette = None;
        self.work_outline_modal = Some(modal);
        self.work_outline_modal_focus_pending = true;
        cx.notify();
    }

    /// Close the modal as cancelled. Returns whether one was open.
    pub(super) fn cancel_work_outline_modal(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.resolve_work_outline_modal(false, window, cx)
    }

    fn resolve_work_outline_modal(
        &mut self,
        accept: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(modal) = self.work_outline_modal.take() else {
            return false;
        };
        self.work_outline_modal_focus_pending = false;
        if let Some(prior) = self.work_outline_prior_focus.take() {
            window.focus(&prior, cx);
        }
        cx.notify();
        window.defer(cx, move |window, cx| {
            deliver_decision(modal, accept, window, cx)
        });
        true
    }

    /// Single entry point for the Work Outline commands (palette, keymap):
    /// open the Overview route if needed, then let the Overview act.
    pub(crate) fn run_work_outline_command(
        &mut self,
        command: WorkOutlineCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.command_palette = None;
        if !matches!(self.route, Route::Overview) {
            self.open_overview(cx);
        }
        let page = self.ensure_overview_page(cx);
        page.update(cx, |overview, cx| {
            overview.run_work_outline_command(command, window, cx)
        });
    }

    pub(super) fn render_work_outline_modal(
        &mut self,
        viewport: gpui::Size<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        for superseded in std::mem::take(&mut self.work_outline_superseded) {
            window.defer(cx, move |window, cx| {
                deliver_decision(superseded, false, window, cx)
            });
        }
        let theme = Theme::of(cx).for_popup();
        let modal = self.work_outline_modal.as_ref()?;
        // Move keyboard focus into the modal (remembering where it was), so
        // the outline's key actions (Enter, n, ...) cannot fire underneath it.
        if std::mem::take(&mut self.work_outline_modal_focus_pending) {
            if self.work_outline_prior_focus.is_none() {
                self.work_outline_prior_focus = window
                    .focused(cx)
                    .filter(|focused| *focused != self.work_outline_modal_focus);
            }
            window.focus(&self.work_outline_modal_focus, cx);
        }
        let card = match modal {
            WorkOutlineModal::Confirm(dialog) => {
                let accept = if dialog.destructive {
                    popover::btn_danger(&theme, &dialog.confirm_label)
                } else {
                    popover::btn_primary(&theme, &dialog.confirm_label)
                }
                .id("work-outline-confirm-accept")
                .debug_selector(|| "work-outline-confirm-accept".into())
                .on_click(cx.listener(|this, _, window, cx| {
                    this.resolve_work_outline_modal(true, window, cx);
                }));
                popover::dialog_card(&theme)
                    .child(popover::dialog_title(&theme, &dialog.title))
                    .child(
                        div()
                            .mt(px(6.0))
                            .child(popover::dialog_body(&theme, dialog.body.clone())),
                    )
                    .child(
                        div()
                            .mt(px(16.0))
                            .flex()
                            .flex_row()
                            .justify_end()
                            .gap(px(8.0))
                            .child(
                                popover::btn_ghost(
                                    &theme,
                                    &dialog.cancel_label,
                                    "work-outline-confirm-cancel",
                                )
                                .id("work-outline-confirm-cancel")
                                .debug_selector(|| "work-outline-confirm-cancel".into())
                                .on_click(cx.listener(
                                    |this, _, window, cx| {
                                        this.resolve_work_outline_modal(false, window, cx);
                                    },
                                )),
                            )
                            .child(accept),
                    )
                    .into_any_element()
            }
            WorkOutlineModal::Archive(dialog) => {
                let policy = dialog.policy();
                let has_descendants = dialog.descendant_count > 0;
                let mut card = popover::dialog_card(&theme)
                    .child(popover::dialog_title(&theme, "Archive work item?"));
                let body = if has_descendants {
                    format!(
                        "\u{201C}{}\u{201D} has {} descendant work item{}. Choose what happens to them.",
                        dialog.item_title,
                        dialog.descendant_count,
                        if dialog.descendant_count == 1 {
                            ""
                        } else {
                            "s"
                        }
                    )
                } else {
                    format!(
                        "\u{201C}{}\u{201D} will be archived. You can restore it later.",
                        dialog.item_title
                    )
                };
                card = card.child(div().mt(px(6.0)).child(popover::dialog_body(&theme, body)));
                if has_descendants {
                    let option = |id: &'static str,
                                  label: String,
                                  selected: bool,
                                  choice: ArchiveChoice,
                                  cx: &mut Context<Self>| {
                        popover::menu_row(&theme, selected, id)
                            .id(id)
                            .debug_selector(move || id.into())
                            .role(gpui::Role::RadioButton)
                            .aria_label(label.clone())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(WorkOutlineModal::Archive(dialog)) =
                                    this.work_outline_modal.as_mut()
                                {
                                    dialog.choice = Some(choice);
                                    cx.notify();
                                }
                            }))
                            .child(icon(icons::CHECK).size(px(14.0)).flex_none().text_color(
                                if selected {
                                    theme.text
                                } else {
                                    gpui::transparent_black()
                                },
                            ))
                            .child(div().flex_1().min_w_0().child(SharedString::from(label)))
                    };
                    let archive_selected = dialog.choice == Some(ArchiveChoice::ArchiveDescendants);
                    let move_selected = dialog.choice == Some(ArchiveChoice::MoveDescendants);
                    let mut options = div().mt(px(12.0)).flex().flex_col().gap(px(2.0));
                    options = options.child(option(
                        "work-outline-archive-descendants",
                        "Archive the descendants too".into(),
                        archive_selected,
                        ArchiveChoice::ArchiveDescendants,
                        cx,
                    ));
                    options = options.child(option(
                        "work-outline-move-descendants",
                        "Move the descendants to another parent first".into(),
                        move_selected,
                        ArchiveChoice::MoveDescendants,
                        cx,
                    ));
                    if move_selected {
                        let mut list = div()
                            .id("work-outline-parent-list")
                            .ml(px(22.0))
                            .max_h(px(180.0))
                            .overflow_y_scroll()
                            .flex()
                            .flex_col();
                        for (ix, parent) in dialog.parents.iter().enumerate() {
                            let selected = ix == dialog.parent_ix;
                            list = list.child(
                                popover::menu_row(
                                    &theme,
                                    selected,
                                    format!("work-outline-parent-{ix}"),
                                )
                                .id(("work-outline-parent", ix))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(WorkOutlineModal::Archive(dialog)) =
                                        this.work_outline_modal.as_mut()
                                    {
                                        dialog.parent_ix = ix;
                                        cx.notify();
                                    }
                                }))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .child(parent.label.clone()),
                                ),
                            );
                        }
                        options = options.child(list);
                    }
                    card = card.child(options);
                }
                let confirm_label = match dialog.choice {
                    Some(ArchiveChoice::MoveDescendants) => "Move and archive",
                    _ => "Archive",
                };
                let confirm = if policy.is_some() {
                    popover::btn_danger(&theme, confirm_label)
                        .id("work-outline-archive-confirm")
                        .debug_selector(|| "work-outline-archive-confirm".into())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.resolve_work_outline_modal(true, window, cx);
                        }))
                        .into_any_element()
                } else {
                    // No valid choice yet: visibly inert, no handler.
                    popover::btn_danger(&theme, confirm_label)
                        .id("work-outline-archive-confirm-disabled")
                        .opacity(0.4)
                        .cursor_default()
                        .into_any_element()
                };
                card.child(
                    div()
                        .mt(px(16.0))
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap(px(8.0))
                        .child(
                            popover::btn_ghost(&theme, "Cancel", "work-outline-archive-cancel")
                                .id("work-outline-archive-cancel")
                                .debug_selector(|| "work-outline-archive-cancel".into())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.resolve_work_outline_modal(false, window, cx);
                                })),
                        )
                        .child(confirm),
                )
                .into_any_element()
            }
        };
        let card = div()
            .track_focus(&self.work_outline_modal_focus)
            .child(card)
            .into_any_element();
        Some(popover::modal("work-outline-modal", viewport, card))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parents() -> Vec<WorkOutlineParentChoice> {
        vec![
            WorkOutlineParentChoice {
                work_item_id: None,
                label: "Space root".into(),
            },
            WorkOutlineParentChoice {
                work_item_id: Some("w-api".into()),
                label: "Launch v2 / API".into(),
            },
        ]
    }

    fn archive_dialog(descendants: usize) -> ArchiveDialog {
        ArchiveDialog::new(WorkOutlineArchiveRequest {
            item_title: "Launch v2".into(),
            descendant_count: descendants,
            parents: parents(),
            on_decision: Box::new(|_, _, _| {}),
        })
    }

    #[test]
    fn archive_with_descendants_requires_an_explicit_choice() {
        let mut dialog = archive_dialog(3);
        assert_eq!(dialog.policy(), None);
        dialog.choice = Some(ArchiveChoice::ArchiveDescendants);
        assert_eq!(
            dialog.policy(),
            Some(DescendantArchivePolicy::ArchiveDescendants)
        );
        dialog.choice = Some(ArchiveChoice::MoveDescendants);
        assert_eq!(
            dialog.policy(),
            Some(DescendantArchivePolicy::MoveDescendants {
                destination_parent_work_item_id: None
            })
        );
        dialog.parent_ix = 1;
        assert_eq!(
            dialog.policy(),
            Some(DescendantArchivePolicy::MoveDescendants {
                destination_parent_work_item_id: Some("w-api".into())
            })
        );
    }

    #[test]
    fn archive_without_descendants_needs_no_choice() {
        assert_eq!(
            archive_dialog(0).policy(),
            Some(DescendantArchivePolicy::ArchiveDescendants)
        );
    }

    #[test]
    fn moving_descendants_with_no_destination_is_not_confirmable() {
        let mut dialog = archive_dialog(2);
        dialog.parents.clear();
        dialog.choice = Some(ArchiveChoice::MoveDescendants);
        assert_eq!(dialog.policy(), None);
    }

    #[test]
    fn references_open_externally_unless_a_chat_browser_exists() {
        let url = "https://github.com/acme/launch/pull/42";
        // Overview-hosted details: external, whatever the preference.
        assert!(matches!(
            reference_open(url, false, true, true),
            ReferenceOpen::External(_)
        ));
        assert!(matches!(
            reference_open(url, true, true, true),
            ReferenceOpen::Internal(_)
        ));
        assert!(matches!(
            reference_open(url, true, false, true),
            ReferenceOpen::External(_)
        ));
        assert!(matches!(
            reference_open(url, true, true, false),
            ReferenceOpen::External(_)
        ));
    }

    #[test]
    fn non_web_references_are_rejected() {
        for url in ["file:///etc/passwd", "javascript:alert(1)", "not a url", ""] {
            assert_eq!(
                reference_open(url, false, true, true),
                ReferenceOpen::Rejected,
                "{url}"
            );
        }
    }

    #[test]
    fn navigation_keys_parse_are_unique_and_exclude_text_inputs() {
        for (index, (keys, _)) in OUTLINE_NAVIGATION_KEYS.iter().enumerate() {
            assert!(Keystroke::parse(keys).is_ok(), "{keys}");
            assert!(
                OUTLINE_NAVIGATION_KEYS[index + 1..]
                    .iter()
                    .all(|(other, _)| other != keys),
                "duplicate {keys}"
            );
        }
        let predicate = gpui::KeyBindingContextPredicate::parse(OUTLINE_NAVIGATION_CONTEXT)
            .expect("context predicate parses");
        let matches = |names: &[&str]| {
            let mut context = gpui::KeyContext::default();
            for name in names {
                context.add(*name);
            }
            predicate.eval(&[context])
        };
        assert!(matches(&["WorkOutline"]));
        for input in TEXT_INPUT_CONTEXTS {
            assert!(!matches(&["WorkOutline", input]), "{input}");
            assert!(OUTLINE_NAVIGATION_CONTEXT.contains(&format!("!{input}")));
        }
        assert!(!matches(&["Terminal"]));
    }

    #[test]
    fn command_bindings_skip_unbound_and_fall_back_for_unparseable() {
        let mut keymap = KeymapConfig::default();
        // Defaults: only Open Outline and Create Work Item ship bound.
        assert_eq!(work_outline_bindings(&keymap).len(), 2);
        keymap.set(ShortcutId::RenameWorkItem, "mod-shift-m".into());
        assert_eq!(work_outline_bindings(&keymap).len(), 3);
        // Unparseable with a default: falls back to it.
        keymap.set(ShortcutId::OpenOutline, "mod-bogus-???".into());
        assert_eq!(work_outline_bindings(&keymap).len(), 3);
        // Unparseable without a default: unbound, never a panic.
        keymap.set(ShortcutId::MoveWorkItem, "mod-bogus-???".into());
        assert_eq!(work_outline_bindings(&keymap).len(), 3);
        keymap.set(ShortcutId::OpenOutline, String::new());
        assert_eq!(work_outline_bindings(&keymap).len(), 2);
    }

    fn test_shell<'a>(
        cx: &'a mut gpui::TestAppContext,
        dir: &std::path::Path,
    ) -> (Entity<Shell>, &'a mut gpui::VisualTestContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
        });
        let data_dir = dir.to_path_buf();
        cx.add_window_view(move |_, cx| {
            let state = cx.new(|_| AppState::new());
            state.update(cx, |state, _| {
                // The overlays render only behind the Ready gate.
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
        })
    }

    fn confirm_request(
        decisions: std::rc::Rc<std::cell::RefCell<Vec<bool>>>,
    ) -> WorkOutlineConfirmRequest {
        WorkOutlineConfirmRequest {
            title: "Discard changes?".into(),
            body: "Your edits to this work item are unsaved.".into(),
            confirm_label: "Discard".into(),
            cancel_label: "Keep editing".into(),
            destructive: true,
            on_decision: Box::new(move |accepted, _, _| decisions.borrow_mut().push(accepted)),
        }
    }

    fn escape() -> gpui::KeyDownEvent {
        gpui::KeyDownEvent {
            keystroke: Keystroke::parse("escape").unwrap(),
            is_held: false,
            prefer_character_input: false,
        }
    }

    #[gpui::test]
    fn escape_cancels_the_confirm_modal_and_reports_false(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let decisions = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        shell.update(cx, |shell, cx| {
            assert!(!shell.overlay_owns_keyboard(cx));
            shell.open_work_outline_confirm(confirm_request(decisions.clone()), cx);
            assert!(shell.overlay_owns_keyboard(cx), "modal owns the keyboard");
            assert!(
                shell.capture_escape_surface(cx),
                "escape is a shell surface"
            );
        });
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        assert!(cx.debug_bounds("work-outline-confirm-accept").is_some());
        shell.update_in(cx, |shell, window, cx| {
            shell.on_key_down_capture(&escape(), window, cx)
        });
        cx.run_until_parked();
        assert_eq!(*decisions.borrow(), [false]);
        shell.update(cx, |shell, cx| {
            assert!(shell.work_outline_modal.is_none());
            assert!(!shell.overlay_owns_keyboard(cx));
        });
        // A second Escape reports nothing more.
        shell.update_in(cx, |shell, window, cx| {
            shell.on_key_down_capture(&escape(), window, cx)
        });
        cx.run_until_parked();
        assert_eq!(*decisions.borrow(), [false]);
    }

    #[gpui::test]
    fn confirm_button_reports_true_and_cancel_reports_false(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let decisions = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        for (selector, expected) in [
            ("work-outline-confirm-accept", true),
            ("work-outline-confirm-cancel", false),
        ] {
            shell.update(cx, |shell, cx| {
                shell.open_work_outline_confirm(confirm_request(decisions.clone()), cx)
            });
            cx.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear();
            });
            let center = cx.debug_bounds(selector).unwrap().center();
            cx.simulate_click(center, gpui::Modifiers::default());
            cx.run_until_parked();
            assert_eq!(decisions.borrow().last(), Some(&expected), "{selector}");
        }
        assert_eq!(decisions.borrow().len(), 2);
    }

    #[gpui::test]
    fn a_newer_modal_supersedes_the_open_one_with_a_cancel(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let first = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let second = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        shell.update(cx, |shell, cx| {
            shell.open_work_outline_confirm(confirm_request(first.clone()), cx);
            shell.open_work_outline_confirm(confirm_request(second.clone()), cx);
        });
        cx.run_until_parked();
        assert_eq!(*first.borrow(), [false]);
        assert!(second.borrow().is_empty());
        shell.update(cx, |shell, cx| assert!(shell.overlay_owns_keyboard(cx)));
    }

    #[gpui::test]
    fn archive_modal_keeps_confirm_disabled_until_a_choice(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let decisions: std::rc::Rc<std::cell::RefCell<Vec<Option<DescendantArchivePolicy>>>> =
            Default::default();
        let sink = decisions.clone();
        shell.update(cx, |shell, cx| {
            shell.open_work_outline_archive_confirm(
                WorkOutlineArchiveRequest {
                    item_title: "Launch v2".into(),
                    descendant_count: 2,
                    parents: parents(),
                    on_decision: Box::new(move |policy, _, _| sink.borrow_mut().push(policy)),
                },
                cx,
            )
        });
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        assert!(
            cx.debug_bounds("work-outline-archive-confirm").is_none(),
            "no confirm target without an explicit choice"
        );
        let center = cx
            .debug_bounds("work-outline-move-descendants")
            .unwrap()
            .center();
        cx.simulate_click(center, gpui::Modifiers::default());
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        let confirm = cx
            .debug_bounds("work-outline-archive-confirm")
            .expect("choice enables confirm")
            .center();
        cx.simulate_click(confirm, gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(
            *decisions.borrow(),
            [Some(DescendantArchivePolicy::MoveDescendants {
                destination_parent_work_item_id: None
            })]
        );
    }

    #[gpui::test]
    fn commands_route_to_the_overview(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        shell.update_in(cx, |shell, window, cx| {
            assert!(!matches!(shell.route, Route::Overview));
            shell.run_work_outline_command(WorkOutlineCommand::OpenOutline, window, cx);
            assert!(matches!(shell.route, Route::Overview));
            assert!(shell.overview_page.is_some());
        });
    }

    #[gpui::test]
    fn modal_takes_focus_so_outline_keys_do_nothing_and_cancel_restores_it(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::work_outline::model::row_key;
        use crate::work_outline::outline_view::{CreateSibling, OpenSelected};
        use crate::work_outline::store::fixture_ids::LAUNCH;

        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                state.spaces = vec![
                    serde_json::from_value(serde_json::json!({
                        "id": "s1", "deviceId": "d", "path": "/work/alpha",
                        "createdAt": "2026-01-01T00:00:00Z"
                    }))
                    .unwrap(),
                ];
            })
        });
        shell.update_in(cx, |shell, window, cx| {
            shell.run_work_outline_command(WorkOutlineCommand::OpenOutline, window, cx);
            let overview = shell.overview_page.clone().unwrap();
            overview.update(cx, |overview, cx| {
                overview.show_work_outline_fixture(window, cx)
            });
        });
        let overview = shell.read_with(cx, |shell, _| shell.overview_page.clone().unwrap());
        let outline = overview.read_with(cx, |overview, _| overview.outline_entity());
        let draw = |cx: &mut gpui::VisualTestContext| {
            cx.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear();
            });
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear());
        };
        draw(cx);
        let outline_focus = cx.update(|_, cx| gpui::Focusable::focus_handle(outline.read(cx), cx));
        outline.update(cx, |outline, cx| {
            outline.select(&row_key::item("s1", LAUNCH), cx)
        });
        cx.update(|window, _| assert!(outline_focus.is_focused(window), "outline starts focused"));

        // Sanity: with no modal the outline's key actions do act.
        cx.update(|window, cx| window.dispatch_action(Box::new(CreateSibling), cx));
        outline.read_with(cx, |outline, _| assert!(outline.is_editing()));
        outline.update(cx, |outline, cx| outline.cancel_edit(cx));
        cx.update(|window, cx| window.focus(&outline_focus, cx));
        draw(cx);

        let decisions = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        shell.update(cx, |shell, cx| {
            shell.open_work_outline_confirm(confirm_request(decisions.clone()), cx)
        });
        draw(cx);
        cx.update(|window, _| {
            assert!(!outline_focus.is_focused(window), "modal took focus");
        });
        cx.update(|window, cx| {
            window.dispatch_action(Box::new(OpenSelected), cx);
            window.dispatch_action(Box::new(CreateSibling), cx);
        });
        cx.run_until_parked();
        overview.read_with(cx, |overview, _| assert!(!overview.has_work_item_details()));
        outline.read_with(cx, |outline, _| assert!(!outline.is_editing()));

        // Cancel restores focus to the outline, and its keys work again.
        shell.update_in(cx, |shell, window, cx| {
            shell.cancel_work_outline_modal(window, cx);
        });
        cx.run_until_parked();
        draw(cx);
        assert_eq!(*decisions.borrow(), [false]);
        cx.update(|window, _| assert!(outline_focus.is_focused(window), "focus restored"));
        cx.update(|window, cx| window.dispatch_action(Box::new(OpenSelected), cx));
        cx.run_until_parked();
        overview.read_with(cx, |overview, _| assert!(overview.has_work_item_details()));
    }
}
