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
use crate::settings::{KeymapConfig, platform_combo};
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
}

/// The stored (platform-neutral, `mod-…`) combo a command is bound to, with
/// the same resolution `work_outline_bindings` binds: empty = unbound, an
/// unparseable combo falls back to the default (or to unbound without one).
pub(crate) fn command_combo(keymap: &KeymapConfig, command: WorkOutlineCommand) -> Option<String> {
    let id = command.shortcut_id();
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

/// The shortcut reference against the live keymap: the Outline keys grouped
/// as Navigate / Edit / Restructure (table order), then every Work Outline
/// command with its current binding.
pub(crate) fn shortcut_reference(keymap: &KeymapConfig) -> Vec<ShortcutGroup> {
    shortcut_reference_on(cfg!(target_os = "macos"), keymap)
}

/// [`shortcut_reference`] for an explicit platform.
pub(crate) fn shortcut_reference_on(mac: bool, keymap: &KeymapConfig) -> Vec<ShortcutGroup> {
    let outline_group = |title: &'static str, group: OutlineKeyGroup| ShortcutGroup {
        title,
        rows: OUTLINE_NAVIGATION_KEYS
            .iter()
            .filter(|(_, key)| key.group() == group)
            .map(|(combo, key)| ShortcutRow {
                label: key.label(),
                keys: Some(key_label_on(mac, combo)),
            })
            .collect(),
    };
    vec![
        outline_group("Navigate", OutlineKeyGroup::Navigate),
        outline_group("Edit", OutlineKeyGroup::Edit),
        outline_group("Restructure", OutlineKeyGroup::Restructure),
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
    ]
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
    use crate::settings::ShortcutId;

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
            Some("Alt+Shift+\u{2191}")
        );
        for (combo, key) in OUTLINE_NAVIGATION_KEYS {
            assert_eq!(outline_key_combo(key), Some(combo));
        }
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
        assert_eq!(titles, ["Navigate", "Edit", "Restructure", "Commands"]);
        let outline_rows: usize = groups[..3].iter().map(|group| group.rows.len()).sum();
        assert_eq!(outline_rows, OUTLINE_NAVIGATION_KEYS.len());
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
        assert_eq!(row(&groups, "Indent").keys.as_deref(), Some("\u{21e5}"));
        assert_eq!(
            row(&groups, "Create Work Item").keys.as_deref(),
            Some("\u{21e7}\u{2318}I")
        );
        assert_eq!(row(&groups, "Show Outline Shortcuts").keys, None);
        keymap.set(ShortcutId::ShowOutlineShortcuts, "mod-alt-/".into());
        let groups = shortcut_reference_on(false, &keymap);
        assert_eq!(
            row(&groups, "Show Outline Shortcuts").keys.as_deref(),
            Some("Ctrl+Alt+/")
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
