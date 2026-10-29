//! Discoverability for the Work Outline: hover tooltips that name a control
//! and carry its CURRENT key binding.
//!
//! Bindings are read when the tooltip is built, from the live settings
//! keymap (rebindable [`WorkOutlineCommand`] shortcuts) or the fixed
//! Outline-context table (`shell::work_outline_ui::OUTLINE_NAVIGATION_KEYS`),
//! so a rebind shows up on the next hover without re-rendering anything. An
//! unbound command shows its label alone. Keys are spelled per platform:
//! modifier glyphs on macOS (`⌃⌥⇧⌘`), `Ctrl+Alt+Shift+` elsewhere.
//!
//! Everything here is on demand (hover only): no permanent instructional
//! text (spec "Apply progressive disclosure").
use std::time::Duration;

use gpui::{
    AnyView, App, Keystroke, SharedString, StatefulInteractiveElement, Window, div, prelude::*, px,
};

use super::command::WorkOutlineCommand;
use super::outline_view::context_menu::OPEN_MENU_KEYS;
use crate::settings::{KeymapConfig, ShortcutId, platform_combo};
use crate::shell::work_outline_ui::{OUTLINE_NAVIGATION_KEYS, OutlineKeyGroup, OutlineNavKey};
use crate::theme::Theme;

/// The app's tooltip delay (the same 350 ms the composer, queue, history and
/// sidebar tooltips use).
pub(crate) const TOOLTIP_DELAY: Duration = Duration::from_millis(350);

/// Which binding a control's tooltip shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HintKey {
    /// A label-only tooltip: the control has no keyboard equivalent.
    None,
    /// A rebindable Work Outline command (empty combo = unbound = no key).
    Command(WorkOutlineCommand),
    /// A fixed Outline-context key.
    Outline(OutlineNavKey),
    /// An app-wide rebindable shortcut that also acts on the selected
    /// Outline row ([`OUTLINE_APP_KEYS`]).
    App(ShortcutId),
    /// Find: the fixed app-wide key that focuses the Overview's search.
    Find,
}

/// App-wide shortcuts that, while the Outline tree has focus, act on the
/// selected row instead of the open Chat (shortcut reference rows).
pub(crate) const OUTLINE_APP_KEYS: [(&str, ShortcutId); 1] =
    [("Archive Chat / work item", ShortcutId::ArchiveSession)];

/// The stored (platform-neutral, `mod-…`) combo a command is bound to, with
/// the same resolution `work_outline_bindings` binds: empty = unbound, an
/// unparseable combo falls back to the default (or to unbound without one).
pub(crate) fn command_combo(keymap: &KeymapConfig, command: WorkOutlineCommand) -> Option<String> {
    shortcut_combo(keymap, command.shortcut_id())
}

/// [`command_combo`] for any rebindable shortcut.
pub(crate) fn shortcut_combo(keymap: &KeymapConfig, id: ShortcutId) -> Option<String> {
    let combo = keymap.get(id);
    if combo.is_empty() {
        return None;
    }
    if Keystroke::parse(&platform_combo(combo)).is_ok() {
        return Some(combo.to_string());
    }
    let fallback = id.default_combo();
    (!fallback.is_empty()).then(|| fallback.to_string())
}

/// The Outline-context key bound to `key`, from the live table.
pub(crate) fn outline_key_combo(key: OutlineNavKey) -> Option<&'static str> {
    OUTLINE_NAVIGATION_KEYS
        .iter()
        .find(|(_, bound)| *bound == key)
        .map(|(combo, _)| *combo)
}

/// The formatted key for `hint` under `keymap`, `None` when unbound.
pub(crate) fn hint_keys(keymap: &KeymapConfig, hint: HintKey) -> Option<String> {
    hint_keys_on(cfg!(target_os = "macos"), keymap, hint)
}

/// [`hint_keys`] for an explicit platform.
pub(crate) fn hint_keys_on(mac: bool, keymap: &KeymapConfig, hint: HintKey) -> Option<String> {
    match hint {
        HintKey::None => None,
        HintKey::Command(command) => {
            command_combo(keymap, command).map(|combo| key_label_on(mac, &combo))
        }
        HintKey::Outline(key) => outline_key_combo(key).map(|combo| key_label_on(mac, combo)),
        HintKey::App(id) => shortcut_combo(keymap, id).map(|combo| key_label_on(mac, &combo)),
        HintKey::Find => Some(key_label_on(mac, crate::shell::FOCUS_OVERVIEW_SEARCH_COMBO)),
    }
}

/// A combo as the user reads it: `mod-shift-i` is `⇧⌘I` on macOS and
/// `Ctrl+Shift+I` elsewhere; arrows are arrows everywhere.
pub(crate) fn key_label(combo: &str) -> String {
    key_label_on(cfg!(target_os = "macos"), combo)
}

/// [`key_label`] for an explicit platform.
pub(crate) fn key_label_on(mac: bool, combo: &str) -> String {
    let mut parts: Vec<&str> = combo.split('-').collect();
    let key = parts.pop().unwrap_or("");
    let has = |name: &str| parts.contains(&name);
    let ctrl = has("ctrl") || (!mac && has("mod"));
    let cmd = has("cmd") || (mac && has("mod"));
    let alt = has("alt");
    let shift = has("shift");
    let key = key_name(mac, key);
    if mac {
        let mut out = String::new();
        for (on, glyph) in [(ctrl, '⌃'), (alt, '⌥'), (shift, '⇧'), (cmd, '⌘')] {
            if on {
                out.push(glyph);
            }
        }
        out.push_str(&key);
        out
    } else {
        let mut out: Vec<String> = Vec::new();
        for (on, name) in [
            (ctrl, "Ctrl"),
            (alt, "Alt"),
            (shift, "Shift"),
            (cmd, "Super"),
        ] {
            if on {
                out.push(name.to_string());
            }
        }
        out.push(key);
        out.join("+")
    }
}

fn key_name(mac: bool, key: &str) -> String {
    let named = match key {
        "up" => Some("\u{2191}"),
        "down" => Some("\u{2193}"),
        "left" => Some("\u{2190}"),
        "right" => Some("\u{2192}"),
        "enter" => Some(if mac { "\u{21a9}" } else { "Enter" }),
        "tab" => Some(if mac { "\u{21e5}" } else { "Tab" }),
        "escape" => Some("Esc"),
        "space" => Some("Space"),
        "backspace" => Some(if mac { "\u{232b}" } else { "Backspace" }),
        "delete" => Some(if mac { "\u{2326}" } else { "Delete" }),
        "pageup" => Some("Page Up"),
        "pagedown" => Some("Page Down"),
        _ => None,
    };
    if let Some(named) = named {
        return named.to_string();
    }
    let mut chars = key.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// The tooltip's text: the label, then the key when one is bound.
pub(crate) fn tooltip_text(label: &str, keys: Option<&str>) -> String {
    match keys {
        Some(keys) => format!("{label} ({keys})"),
        None => label.to_string(),
    }
}

/// The tooltip view: the label and, when bound, the key in the app's
/// keyboard-hint chip. Theme colors only.
pub(crate) struct HintTooltip {
    pub label: SharedString,
    pub keys: Option<SharedString>,
}

impl HintTooltip {
    /// Plain-text form (accessibility, tests).
    pub(crate) fn text(&self) -> String {
        tooltip_text(&self.label, self.keys.as_deref())
    }
}

impl Render for HintTooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).for_popup();
        div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .px(px(8.0))
            .py(px(4.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface_dialog)
            .text_size(crate::typography::ui_rems(11.0))
            .text_color(theme.text)
            .child(self.label.clone())
            .when_some(self.keys.clone(), |el, keys| {
                el.child(crate::popover::kbd_hint(&theme, &keys))
            })
    }
}

/// Build the tooltip for `label` + `hint` against the live keymap.
pub(crate) fn hint_tooltip_view(label: SharedString, hint: HintKey, cx: &mut App) -> AnyView {
    let keymap = crate::settings::current(cx).keymap;
    let keys = hint_keys(&keymap, hint).map(SharedString::from);
    cx.new(|_| HintTooltip { label, keys }).into()
}

/// Attach a hint tooltip (label + live binding) with the app tooltip delay.
pub(crate) trait WithHint: StatefulInteractiveElement + Sized {
    fn hint(self, label: impl Into<SharedString>, key: HintKey) -> Self {
        let label = label.into();
        self.tooltip(move |_, cx| hint_tooltip_view(label.clone(), key, cx))
            .tooltip_show_delay(TOOLTIP_DELAY)
    }
}

impl<E: StatefulInteractiveElement + Sized> WithHint for E {}

/// One line of the shortcut reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShortcutRow {
    pub label: &'static str,
    /// Formatted key; `None` = unbound (the row shows no key).
    pub keys: Option<String>,
}

/// A titled group of the shortcut reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShortcutGroup {
    pub title: &'static str,
    pub rows: Vec<ShortcutRow>,
}

/// The restructuring keys' Chat-row meaning (shortcut reference rows):
/// Shift+Up / Down / Left (and Shift+Tab) re-associate the selected Chat;
/// Shift+Right / Tab do nothing on a Chat row.
pub(crate) const CHAT_MOVE_KEYS: [(&str, OutlineNavKey); 3] = [
    ("Chat to previous item", OutlineNavKey::MoveItemUp),
    ("Chat to next item", OutlineNavKey::MoveItemDown),
    ("Chat to parent item", OutlineNavKey::OutdentItem),
];

/// The restructuring keys on a project (Space root) row of a multi-project
/// Outline: Shift+Up / Down reorder the project roots (this device only).
pub(crate) const SPACE_MOVE_KEYS: [(&str, OutlineNavKey); 2] = [
    ("Project up (project row)", OutlineNavKey::MoveItemUp),
    ("Project down (project row)", OutlineNavKey::MoveItemDown),
];

/// The shortcut reference against the live keymap: the Outline keys grouped
/// as Navigate / Edit / Restructure (table order; Restructure ends with the
/// keys' Chat-row and project-row moves), then every Work Outline command with its current
/// binding.
pub(crate) fn shortcut_reference(keymap: &KeymapConfig) -> Vec<ShortcutGroup> {
    shortcut_reference_on(cfg!(target_os = "macos"), keymap)
}

/// [`shortcut_reference`] for an explicit platform.
pub(crate) fn shortcut_reference_on(mac: bool, keymap: &KeymapConfig) -> Vec<ShortcutGroup> {
    // Every key of `key`, primary first.
    let all_keys = |key: OutlineNavKey| {
        OUTLINE_NAVIGATION_KEYS
            .iter()
            .filter(|(_, other)| *other == key)
            .map(|(combo, _)| key_label_on(mac, combo))
            .collect::<Vec<_>>()
            .join(" / ")
    };
    let outline_group = |title: &'static str, group: OutlineKeyGroup| ShortcutGroup {
        title,
        // One row per action; an action with aliases (Tab for Indent) lists
        // every key, primary first.
        rows: OUTLINE_NAVIGATION_KEYS
            .iter()
            .enumerate()
            .filter(|(index, (_, key))| {
                key.group() == group
                    && OUTLINE_NAVIGATION_KEYS[..*index]
                        .iter()
                        .all(|(_, earlier)| earlier != key)
            })
            .map(|(_, (_, key))| ShortcutRow {
                label: key.label(),
                keys: Some(all_keys(*key)),
            })
            .collect(),
    };
    let mut edit = outline_group("Edit", OutlineKeyGroup::Edit);
    // App shortcuts that act on the selected row in the Outline.
    edit.rows
        .extend(OUTLINE_APP_KEYS.into_iter().map(|(label, id)| ShortcutRow {
            label,
            keys: hint_keys_on(mac, keymap, HintKey::App(id)),
        }));
    let mut restructure = outline_group("Restructure", OutlineKeyGroup::Restructure);
    // On a Chat row the same keys move the Chat between work items.
    restructure
        .rows
        .extend(
            CHAT_MOVE_KEYS
                .into_iter()
                .chain(SPACE_MOVE_KEYS)
                .map(|(label, key)| ShortcutRow {
                    label,
                    keys: Some(all_keys(key)),
                }),
        );
    let mut navigate = outline_group("Navigate", OutlineKeyGroup::Navigate);
    // Find works from anywhere in the Overview; the Outline filters by it.
    navigate.rows.push(ShortcutRow {
        label: "Search",
        keys: hint_keys_on(mac, keymap, HintKey::Find),
    });
    vec![
        navigate,
        edit,
        restructure,
        ShortcutGroup {
            title: "Commands",
            rows: WorkOutlineCommand::ALL
                .into_iter()
                .map(|command| ShortcutRow {
                    label: command.label(),
                    keys: hint_keys_on(mac, keymap, HintKey::Command(command)),
                })
                .collect(),
        },
        linear_group(mac),
        github_group(mac, keymap),
        chat_item_group(mac),
        plan_group(mac),
    ]
}

/// The chat header's keys: the side panel's Item tab and the pinned context
/// card (fixed combos, defined in the Shell).
fn chat_item_group(mac: bool) -> ShortcutGroup {
    use crate::shell::{OPEN_ITEM_TAB_COMBO, TOGGLE_ITEM_CARD_COMBO};
    ShortcutGroup {
        title: "Chat header",
        rows: vec![
            ShortcutRow {
                label: "Open the Item tab",
                keys: Some(key_label_on(mac, OPEN_ITEM_TAB_COMBO)),
            },
            ShortcutRow {
                label: "Pin or release the context card",
                keys: Some(key_label_on(mac, TOGGLE_ITEM_CARD_COMBO)),
            },
        ],
    }
}

/// The plan-reading rows of the shortcut reference (an Artifact tab): the
/// section keys, the section rail's keys and find, from the tab's own key
/// table.
fn plan_group(mac: bool) -> ShortcutGroup {
    use crate::artifacts::tab::find_view as keys;
    let label = |combo: &str| key_label_on(mac, combo);
    let pair = |a: &str, b: &str| format!("{} / {}", label(a), label(b));
    ShortcutGroup {
        title: "Plan",
        rows: vec![
            ShortcutRow {
                label: "Previous / next section",
                keys: Some(pair(keys::SECTION_PREVIOUS_COMBO, keys::SECTION_NEXT_COMBO)),
            },
            ShortcutRow {
                label: "Focus the section rail",
                keys: Some(label(keys::FOCUS_INDEX_COMBO)),
            },
            ShortcutRow {
                label: "In the rail: previous / next / first / last section",
                keys: Some(
                    keys::RAIL_COMBOS
                        .iter()
                        .map(|combo| label(combo))
                        .collect::<Vec<_>>()
                        .join(" "),
                ),
            },
            ShortcutRow {
                label: "Find in the plan",
                keys: Some(label(keys::FIND_OPEN_COMBO)),
            },
            ShortcutRow {
                label: "Next / previous match",
                keys: Some(pair(keys::FIND_NEXT_COMBO, keys::FIND_PREVIOUS_COMBO)),
            },
        ],
    }
}

/// The GitHub rows of the shortcut reference (projects with the PR review
/// sync): ⌘↩ on a PR review item starts a code review of its PR filed under
/// the item; the item menu picks the sync destination and offers Review /
/// Fix CI / Open on GitHub.
fn github_group(mac: bool, keymap: &KeymapConfig) -> ShortcutGroup {
    let right_click = if mac {
        "Right-click / \u{2303}-click"
    } else {
        "Right-click"
    };
    ShortcutGroup {
        title: "GitHub",
        rows: vec![
            ShortcutRow {
                label: "Open the PR page",
                keys: hint_keys_on(mac, keymap, HintKey::App(ShortcutId::OpenMyPrs)),
            },
            ShortcutRow {
                label: "Review the PR (PR review item)",
                keys: Some(key_label_on(
                    mac,
                    &crate::settings::platform_combo_on(mac, "mod-enter"),
                )),
            },
            ShortcutRow {
                label: "Use as PR review destination; Review / Fix CI",
                keys: Some(right_click.into()),
            },
        ],
    }
}

/// The Linear rows of the shortcut reference (projects with Linear on): an
/// issue row creates under its project's linked item (else the selected
/// item); a project-linked item adds issues from its project.
fn linear_group(mac: bool) -> ShortcutGroup {
    ShortcutGroup {
        title: "Linear",
        rows: vec![
            ShortcutRow {
                label: "Create from issue (Linear panel row)",
                keys: Some(format!("{} / Double-click", key_label_on(mac, "enter"))),
            },
            ShortcutRow {
                label: "Add issues from project (project-linked item)",
                keys: Some(
                    if mac {
                        "Right-click / \u{2303}-click"
                    } else {
                        "Right-click"
                    }
                    .into(),
                ),
            },
        ],
    }
}

/// The shortcut reference's footer: where Outline keys work, how to reach a
/// row's context menu (right-click, Control-click on macOS, or the
/// keyboard), and where to rebind.
pub(crate) fn shortcuts_footer() -> String {
    shortcuts_footer_on(cfg!(target_os = "macos"))
}

/// [`shortcuts_footer`] for an explicit platform.
pub(crate) fn shortcuts_footer_on(mac: bool) -> String {
    let menu_key = key_label_on(mac, OPEN_MENU_KEYS[0]);
    let click = if mac {
        "Right-click or Control-click a row"
    } else {
        "Right-click a row"
    };
    format!(
        "Outline keys work while the Outline has focus. {click} (or press {menu_key}) for its \
         actions. Rebind commands in Settings \u{203a} Shortcuts."
    )
}

/// Tooltip label (and binding) for a Work Item Details button, keyed by its
/// visible label so every button built through the shared helpers gets one.
pub(crate) fn details_button_hint(label: &str) -> Option<(&'static str, HintKey)> {
    let label = label.trim_end_matches('\u{2026}').trim_end_matches("...");
    Some(match label {
        "Save" | "Saving" => ("Save changes", HintKey::None),
        "Cancel" => ("Discard changes", HintKey::None),
        "Edit" => ("Edit", HintKey::None),
        "Validate" | "Validating" => ("Check this execution setup on its device", HintKey::None),
        "Reset to inherited" | "Resetting" => (
            "Drop this item\u{2019}s own setup and use the inherited one",
            HintKey::None,
        ),
        "Start Chat" => (
            "Start a Chat in this work item",
            HintKey::Command(WorkOutlineCommand::StartChatForWorkItem),
        ),
        "Archive" => ("Archive", HintKey::None),
        "Restore" => ("Restore this work item", HintKey::None),
        "Approve" | "Approving" => ("Approve the proposed instruction", HintKey::None),
        "Reject" => ("Reject the proposed instruction", HintKey::None),
        "Review" => ("Review the proposed instruction", HintKey::None),
        "Resolve" | "Resolving" => ("Mark this request resolved", HintKey::None),
        "Open" => ("Open the Chat that asked", HintKey::None),
        "Details" => ("Show details", HintKey::None),
        "Hide details" => ("Hide details", HintKey::None),
        "Add instruction" => ("Add an instruction to this scope", HintKey::None),
        "Add reference" => ("Link a PR, ticket, or URL", HintKey::None),
        "Add" => ("Add", HintKey::None),
        "Remove" => ("Remove", HintKey::None),
        "Associate" => (
            "Associate a Chat with this work item",
            HintKey::Command(WorkOutlineCommand::AssociateChatWithWorkItem),
        ),
        "Disassociate" => ("Remove this Chat from the work item", HintKey::None),
        "Choose" => ("Choose a folder", HintKey::None),
        "\u{2191}" => ("Move instruction up", HintKey::None),
        "\u{2193}" => ("Move instruction down", HintKey::None),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_spelled_per_platform() {
        assert_eq!(key_label_on(true, "mod-shift-i"), "\u{21e7}\u{2318}I");
        assert_eq!(key_label_on(false, "mod-shift-i"), "Ctrl+Shift+I");
        assert_eq!(
            key_label_on(true, "alt-shift-up"),
            "\u{2325}\u{21e7}\u{2191}"
        );
        assert_eq!(key_label_on(false, "alt-shift-up"), "Alt+Shift+\u{2191}");
        assert_eq!(
            key_label_on(true, "ctrl-alt-space"),
            "\u{2303}\u{2325}Space"
        );
        assert_eq!(key_label_on(false, "shift-tab"), "Shift+Tab");
        assert_eq!(key_label_on(true, "f2"), "F2");
        assert_eq!(key_label_on(false, "n"), "N");
    }

    #[test]
    fn command_hints_follow_the_configured_binding() {
        let mut keymap = KeymapConfig::default();
        let create = HintKey::Command(WorkOutlineCommand::CreateWorkItem);
        assert_eq!(
            hint_keys_on(true, &keymap, create).as_deref(),
            Some("\u{21e7}\u{2318}I")
        );
        assert_eq!(
            hint_keys_on(false, &keymap, create).as_deref(),
            Some("Ctrl+Shift+I")
        );
        // Rebinding changes the hint.
        keymap.set(ShortcutId::CreateWorkItem, "mod-alt-k".into());
        assert_eq!(
            hint_keys_on(false, &keymap, create).as_deref(),
            Some("Ctrl+Alt+K")
        );
        // Cleared: no key at all.
        keymap.set(ShortcutId::CreateWorkItem, String::new());
        assert_eq!(hint_keys_on(false, &keymap, create), None);
        // Unbound by default: no key.
        let start = HintKey::Command(WorkOutlineCommand::StartChatForWorkItem);
        assert_eq!(hint_keys_on(true, &keymap, start), None);
        // Unparseable falls back to the default, like the bound keymap.
        keymap.set(ShortcutId::OpenOutline, "mod-bogus-???".into());
        assert_eq!(
            hint_keys_on(
                false,
                &keymap,
                HintKey::Command(WorkOutlineCommand::OpenOutline)
            )
            .as_deref(),
            Some("Ctrl+Shift+L")
        );
        assert_eq!(hint_keys_on(true, &keymap, HintKey::None), None);
    }

    #[test]
    fn outline_hints_come_from_the_navigation_table() {
        let keymap = KeymapConfig::default();
        assert_eq!(
            hint_keys_on(true, &keymap, HintKey::Outline(OutlineNavKey::Expand)).as_deref(),
            Some("\u{2192}")
        );
        assert_eq!(
            hint_keys_on(false, &keymap, HintKey::Outline(OutlineNavKey::MoveItemUp)).as_deref(),
            Some("Shift+\u{2191}")
        );
        // Aliases: the tooltip shows the primary key.
        assert_eq!(
            hint_keys_on(true, &keymap, HintKey::Outline(OutlineNavKey::IndentItem)).as_deref(),
            Some("\u{21e7}\u{2192}")
        );
        assert_eq!(
            hint_keys_on(true, &keymap, HintKey::Outline(OutlineNavKey::SelectNext)).as_deref(),
            Some("\u{2193}")
        );
        for (combo, key) in OUTLINE_NAVIGATION_KEYS {
            let primary = OUTLINE_NAVIGATION_KEYS
                .iter()
                .find(|(_, other)| *other == key)
                .map(|(combo, _)| *combo);
            assert_eq!(outline_key_combo(key), primary, "{combo}");
        }
    }

    #[test]
    fn shortcuts_footer_mentions_right_click_and_the_menu_key() {
        let mac = shortcuts_footer_on(true);
        assert!(mac.contains("Right-click or Control-click a row"), "{mac}");
        assert!(mac.contains("\u{21e7}F10"), "{mac}");
        let other = shortcuts_footer_on(false);
        assert!(other.contains("Right-click a row"), "{other}");
        assert!(!other.contains("Control-click"), "{other}");
        assert!(other.contains("Shift+F10"), "{other}");
        assert!(other.contains("Settings \u{203a} Shortcuts"), "{other}");
    }

    #[test]
    fn tooltip_text_includes_the_key_only_when_bound() {
        assert_eq!(
            tooltip_text("New item", Some("\u{21e7}\u{2318}I")),
            "New item (\u{21e7}\u{2318}I)"
        );
        assert_eq!(tooltip_text("Required actions", None), "Required actions");
    }

    #[test]
    fn details_buttons_have_hints_and_start_chat_names_its_command() {
        for label in [
            "Save",
            "Saving\u{2026}",
            "Cancel",
            "Validate",
            "Reset to inherited",
            "Archive\u{2026}",
            "Restore",
            "Approve",
            "Reject\u{2026}",
            "Resolve",
            "Open",
        ] {
            assert!(details_button_hint(label).is_some(), "{label}");
        }
        assert_eq!(
            details_button_hint("Start Chat").map(|(_, key)| key),
            Some(HintKey::Command(WorkOutlineCommand::StartChatForWorkItem))
        );
    }

    #[test]
    fn reference_groups_every_outline_key_and_command_with_live_bindings() {
        let mut keymap = KeymapConfig::default();
        let groups = shortcut_reference_on(true, &keymap);
        let titles: Vec<_> = groups.iter().map(|group| group.title).collect();
        assert_eq!(
            titles,
            [
                "Navigate",
                "Edit",
                "Restructure",
                "Commands",
                "Linear",
                "GitHub",
                "Chat header",
                "Plan"
            ]
        );
        let outline_rows: usize = groups[..3].iter().map(|group| group.rows.len()).sum();
        // Tab / Shift+Tab, R, and the Ctrl aliases of Undo / Redo share rows with
        // Indent / Outdent / Rename / Undo / Redo;
        // the Chat-row moves add their own rows.
        assert_eq!(
            outline_rows,
            OUTLINE_NAVIGATION_KEYS.len() - 5
                + CHAT_MOVE_KEYS.len()
                + SPACE_MOVE_KEYS.len()
                + OUTLINE_APP_KEYS.len()
                + 1
        );
        assert_eq!(groups[3].rows.len(), WorkOutlineCommand::ALL.len());
        let row = |groups: &[ShortcutGroup], label: &str| {
            groups
                .iter()
                .flat_map(|group| group.rows.iter())
                .find(|row| row.label == label)
                .cloned()
                .unwrap()
        };
        assert_eq!(row(&groups, "Show shortcuts").keys.as_deref(), Some("?"));
        assert_eq!(
            row(&groups, "Open details").keys.as_deref(),
            Some("\u{2318}I")
        );
        assert_eq!(row(&groups, "Open").keys.as_deref(), Some("\u{21a9}"));
        assert_eq!(
            hint_keys_on(true, &keymap, HintKey::Outline(OutlineNavKey::OpenDetails)).as_deref(),
            Some("\u{2318}I")
        );
        assert_eq!(
            row(&groups, "Create from issue (Linear panel row)")
                .keys
                .as_deref(),
            Some("\u{21a9} / Double-click")
        );
        assert_eq!(row(&groups, "Search").keys.as_deref(), Some("\u{2318}F"));
        assert_eq!(
            row(&groups, "Project up (project row)").keys.as_deref(),
            Some("\u{21e7}\u{2191}")
        );
        assert_eq!(
            row(&groups, "Archive Chat / work item").keys.as_deref(),
            Some("\u{21e7}\u{2318}A")
        );
        assert_eq!(
            row(&groups, "Indent").keys.as_deref(),
            Some("\u{21e7}\u{2192} / \u{21e5}")
        );
        assert_eq!(
            row(&groups, "Move up").keys.as_deref(),
            Some("\u{21e7}\u{2191}")
        );
        assert_eq!(
            row(&groups, "Next sibling").keys.as_deref(),
            Some("\u{2325}\u{2193}")
        );
        assert_eq!(row(&groups, "Rename").keys.as_deref(), Some("F2 / R"));
        assert_eq!(
            row(&groups, "Chat to next item").keys.as_deref(),
            Some("\u{21e7}\u{2193}")
        );
        assert_eq!(
            row(&groups, "Chat to parent item").keys.as_deref(),
            Some("\u{21e7}\u{2190} / \u{21e7}\u{21e5}")
        );
        assert_eq!(row(&groups, "Next item").keys.as_deref(), Some("\u{2193}"));
        assert_eq!(
            row(&groups, "Create Work Item").keys.as_deref(),
            Some("\u{21e7}\u{2318}I")
        );
        assert_eq!(
            row(&groups, "Open Full Chat").keys.as_deref(),
            Some("\u{21e7}\u{2318}\u{21a9}")
        );
        assert_eq!(
            row(&groups, "Toggle Outline Focus").keys.as_deref(),
            Some("\u{2318};")
        );
        assert_eq!(row(&groups, "Show Outline Shortcuts").keys, None);
        keymap.set(ShortcutId::ShowOutlineShortcuts, "mod-alt-/".into());
        let groups = shortcut_reference_on(false, &keymap);
        assert_eq!(
            row(&groups, "Show Outline Shortcuts").keys.as_deref(),
            Some("Ctrl+Alt+/")
        );
    }

    /// The reference lists the plan keys, with the same combos the Artifact
    /// tab binds, and ⌘P as the PR page.
    #[test]
    fn reference_lists_the_plan_keys_and_the_pr_page() {
        let keymap = KeymapConfig::default();
        let keys = |groups: &[ShortcutGroup], title: &str, label: &str| {
            groups
                .iter()
                .find(|group| group.title == title)
                .unwrap_or_else(|| panic!("{title} group"))
                .rows
                .iter()
                .find(|row| row.label == label)
                .unwrap_or_else(|| panic!("{label} row"))
                .keys
                .clone()
        };
        let mac = shortcut_reference_on(true, &keymap);
        let plain = shortcut_reference_on(false, &keymap);
        for (label, on_mac, elsewhere) in [
            (
                "Previous / next section",
                "\u{2325}\u{2191} / \u{2325}\u{2193}",
                "Alt+\u{2191} / Alt+\u{2193}",
            ),
            ("Focus the section rail", "\u{2325}\u{2318}S", "Ctrl+Alt+S"),
            (
                "In the rail: previous / next / first / last section",
                "\u{2191} \u{2193} Home End",
                "\u{2191} \u{2193} Home End",
            ),
            ("Find in the plan", "\u{2318}F", "Ctrl+F"),
            (
                "Next / previous match",
                "\u{2318}G / \u{21e7}\u{2318}G",
                "Ctrl+G / Ctrl+Shift+G",
            ),
        ] {
            assert_eq!(
                keys(&mac, "Plan", label).as_deref(),
                Some(on_mac),
                "{label}"
            );
            assert_eq!(
                keys(&plain, "Plan", label).as_deref(),
                Some(elsewhere),
                "{label}"
            );
        }
        // The PR page row follows the live binding.
        assert_eq!(
            keys(&mac, "GitHub", "Open the PR page").as_deref(),
            Some("\u{2318}P")
        );
        let mut rebound = KeymapConfig::default();
        rebound.set(ShortcutId::OpenMyPrs, "mod-alt-p".into());
        assert_eq!(
            keys(
                &shortcut_reference_on(true, &rebound),
                "GitHub",
                "Open the PR page"
            )
            .as_deref(),
            Some("\u{2325}\u{2318}P")
        );
        rebound.set(ShortcutId::OpenMyPrs, String::new());
        assert_eq!(
            keys(
                &shortcut_reference_on(true, &rebound),
                "GitHub",
                "Open the PR page"
            ),
            None
        );
    }

    /// `docs/work-outline.md` spells every Outline key and every default
    /// command binding exactly as the app shows them, on both platforms.
    #[test]
    fn user_guide_bindings_match_the_code_defaults() {
        let doc = include_str!("../../../../docs/work-outline.md");
        for mac in [true, false] {
            for (combo, key) in OUTLINE_NAVIGATION_KEYS {
                let label = key_label_on(mac, combo);
                assert!(doc.contains(&format!("`{label}`")), "{key:?}: `{label}`");
            }
            for (label, id) in OUTLINE_APP_KEYS {
                let key = key_label_on(mac, id.default_combo_on(mac));
                assert!(doc.contains(&format!("`{key}`")), "{label}: `{key}`");
            }
            for command in WorkOutlineCommand::ALL {
                let combo = command.shortcut_id().default_combo_on(mac);
                if !combo.is_empty() {
                    let label = key_label_on(mac, combo);
                    assert!(
                        doc.contains(&format!("`{label}`")),
                        "{command:?}: `{label}`"
                    );
                }
            }
        }
    }

    #[gpui::test]
    fn tooltip_view_reads_the_live_keymap(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            cx.set_global(Theme::default());
            crate::settings::init(crate::settings::UiSettings::default(), dir.path(), cx);
        });
        let text = |cx: &mut gpui::TestAppContext| {
            cx.update(|cx| {
                let view = hint_tooltip_view(
                    "New item".into(),
                    HintKey::Command(WorkOutlineCommand::CreateWorkItem),
                    cx,
                );
                view.downcast::<HintTooltip>().unwrap().read(cx).text()
            })
        };
        let default = key_label("mod-shift-i");
        assert_eq!(text(cx), format!("New item ({default})"));
        cx.update(|cx| {
            crate::settings::update(crate::settings::SavePolicy::Debounced, cx, |s| {
                s.keymap.set(ShortcutId::CreateWorkItem, "mod-alt-k".into())
            });
        });
        assert_eq!(text(cx), format!("New item ({})", key_label("mod-alt-k")));
        cx.update(|cx| {
            crate::settings::update(crate::settings::SavePolicy::Debounced, cx, |s| {
                s.keymap.set(ShortcutId::CreateWorkItem, String::new())
            });
        });
        assert_eq!(text(cx), "New item");
    }
}
