//! `/` skill autocomplete for standalone [`ComposerInput`]s outside the chat
//! composer: the Work Outline instruction editors (Work Item Details
//! Instructions and Space-root instructions, Create Instruction from Chat,
//! proposal review) and the Start Chat prompt.
//!
//! It reuses the chat composer's slash machinery rather than a copy of it:
//! the same token rule ([`slash_token`]), the same discovery RPC and params
//! (`ListCommands` for a harness + folder — user-scoped skills plus that
//! folder's project skills, [`list_commands_params`]), the same cache TTL,
//! local ranking, row rendering ([`slash_row_body`]), and the input's
//! completion key routing (arrows move, Enter/Tab accept, Esc dismisses —
//! the input consumes them, so surrounding Outline keys never fire).
//!
//! Only two things vary per surface: the folder ([`SkillSource`], the
//! resolved execution root of the item the text is for) and the accepted
//! text ([`SkillInsertion`]): Start Chat inserts the composer's own `/name`
//! (its first message behaves like a chat message), instruction editors
//! insert plain text an agent reads (`` `name` skill ``) because
//! instructions are delivered as text, never as chat chips.
use std::collections::HashMap;
use std::ops::Range;
use std::time::Instant;

use gpui::{
    Context, Entity, Focusable, MouseButton, ScrollHandle, SharedString, Subscription, Task,
    Window, div, prelude::*, px,
};
use zeron_proto::{ExecutionScope, HarnessId, SlashCommand, WorkOutlineSnapshot};

use super::{
    ComposerInput, ComposerInputEvent, MentionToken, SlashCacheEntry, list_commands_params,
    slash_cache_entry_is_fresh, slash_row_body, slash_token,
};
use crate::state::AppState;
use crate::theme::Theme;

/// The text an accepted row becomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SkillInsertion {
    /// `/name`, exactly what the chat composer's popup inserts.
    SlashCommand,
    /// `` `name` skill ``: plain text for instructions, which agents receive
    /// as text (a `/name` there would read as a path or a chat command).
    InstructionText,
}

impl SkillInsertion {
    pub(crate) fn text(self, name: &str) -> String {
        match self {
            Self::SlashCommand => format!("/{name}"),
            Self::InstructionText => format!("`{name}` skill"),
        }
    }

    /// Whether the list drops the harness's built-in commands: instructions
    /// can only name real skills (an agent cannot run `/compact` from text),
    /// while Start Chat's first message is a chat message and keeps them.
    fn skills_only(self) -> bool {
        matches!(self, Self::InstructionText)
    }
}

/// Built-in harness commands (session controls, not skills). Discovery
/// entries carry no kind field — `SlashCommand` is name / description /
/// argument hint for every harness — so instruction editors filter by this
/// list. Claude Code advertises its built-ins alongside user and project
/// skills; ACP agents and opencode share the common session verbs; Codex
/// discovery lists skills only.
const BUILT_IN_COMMANDS: &[&str] = &[
    "add-dir",
    "agents",
    "bashes",
    "bug",
    "clear",
    "compact",
    "config",
    "context",
    "copy",
    "cost",
    "doctor",
    "effort",
    "exit",
    "export",
    "extra-usage",
    "fast",
    "feedback",
    "heapdump",
    "help",
    "hooks",
    "ide",
    "init",
    "insights",
    "install-github-app",
    "keybindings",
    "login",
    "logout",
    "mcp",
    "memory",
    "migrate-installer",
    "model",
    "new",
    "output-style",
    "permissions",
    "plan",
    "plugin",
    "pr-comments",
    "privacy-settings",
    "quit",
    "release-notes",
    "rename",
    "resume",
    "review",
    "rewind",
    "sandbox",
    "security-review",
    "settings",
    "share",
    "skills",
    "stats",
    "status",
    "statusline",
    "tasks",
    "terminal-setup",
    "theme",
    "todos",
    "undo",
    "upgrade",
    "usage",
    "vim",
];

/// Whether `name` is a built-in harness command rather than a skill.
fn is_built_in_command(name: &str) -> bool {
    BUILT_IN_COMMANDS.contains(&name)
}

/// Where the skill list comes from: the harness that would run the work and
/// the folder it would run in, on the device that owns that folder.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct SkillSource {
    pub harness: HarnessId,
    pub cwd: String,
    pub device_id: Option<String>,
}

impl SkillSource {
    /// The source for text about `item_id` (the Space root when `None` or
    /// empty): the item's resolved execution root (`resolve_execution`),
    /// else the Space path.
    pub(crate) fn for_outline_scope(
        snapshot: &WorkOutlineSnapshot,
        space_id: &str,
        item_id: Option<&str>,
        harness: HarnessId,
        space_path: &str,
        space_device: &str,
    ) -> Self {
        let scope = match item_id.filter(|id| !id.is_empty()) {
            Some(id) => ExecutionScope::WorkItem {
                work_item_id: id.to_owned(),
            },
            None => ExecutionScope::Space {
                space_id: space_id.to_owned(),
            },
        };
        let resolved = snapshot.resolve_execution(&scope);
        let cwd = resolved
            .as_ref()
            .map(|resolved| resolved.configuration.execution_root.clone())
            .filter(|root| !root.is_empty())
            .unwrap_or_else(|| space_path.to_owned());
        let device_id = resolved
            .map(|resolved| resolved.device_id)
            .filter(|device| !device.is_empty())
            .or_else(|| (!space_device.is_empty()).then(|| space_device.to_owned()));
        Self {
            harness,
            cwd,
            device_id,
        }
    }

    fn cache_key(&self) -> (HarnessId, String) {
        (self.harness, self.cwd.clone())
    }
}

/// The popup state for one input. Render the entity next to the input,
/// inside a `relative()` frame: it draws nothing while closed and anchors
/// the list under that frame while open.
pub(crate) struct SkillCompletion {
    state: Entity<AppState>,
    input: Entity<ComposerInput>,
    insertion: SkillInsertion,
    source: Option<SkillSource>,
    /// Paint above modal dialogs (Start Chat, Create Instruction from Chat).
    over_modal: bool,
    token: Option<MentionToken>,
    /// Source the open popup lists (the cache key of `filtered`).
    listing: Option<SkillSource>,
    filtered: Vec<usize>,
    active: Option<usize>,
    loading: bool,
    error: Option<SharedString>,
    /// A dismissed token stays closed until it is edited.
    dismissed: Option<(Range<usize>, String)>,
    request: u64,
    cache: HashMap<SkillSource, SlashCacheEntry>,
    scroll: ScrollHandle,
    _fetch: Option<Task<()>>,
    _input_events: Subscription,
}

impl SkillCompletion {
    pub(crate) fn new(
        state: Entity<AppState>,
        input: Entity<ComposerInput>,
        insertion: SkillInsertion,
        cx: &mut Context<Self>,
    ) -> Self {
        let input_events = cx.subscribe(&input, |this: &mut Self, _, event, cx| match event {
            ComposerInputEvent::Edited | ComposerInputEvent::CursorMoved => this.refresh(cx),
            ComposerInputEvent::MentionNavigate(delta) => this.move_active(*delta, cx),
            ComposerInputEvent::MentionAccept | ComposerInputEvent::MentionAttach => {
                this.accept(cx)
            }
            ComposerInputEvent::MentionDismiss => this.dismiss(cx),
            _ => {}
        });
        Self {
            state,
            input,
            insertion,
            source: None,
            over_modal: false,
            token: None,
            listing: None,
            filtered: Vec::new(),
            active: None,
            loading: false,
            error: None,
            dismissed: None,
            request: 0,
            cache: HashMap::new(),
            scroll: ScrollHandle::new(),
            _fetch: None,
            _input_events: input_events,
        }
    }

    pub(crate) fn over_modal(mut self) -> Self {
        self.over_modal = true;
        self
    }

    /// Point the list at `source` (cheap when unchanged; an open popup
    /// re-lists for the new folder).
    pub(crate) fn set_source(&mut self, source: SkillSource, cx: &mut Context<Self>) {
        if self.source.as_ref() == Some(&source) {
            return;
        }
        self.source = Some(source);
        if self.token.is_some() {
            self.open_listing(cx);
        }
    }

    #[cfg(test)]
    pub(crate) fn source(&self) -> Option<&SkillSource> {
        self.source.as_ref()
    }

    pub(crate) fn is_open(&self) -> bool {
        self.token.is_some()
    }

    /// Names in the open popup, in display order (tests).
    #[cfg(test)]
    pub(crate) fn visible_names(&self) -> Vec<String> {
        let commands = self.commands();
        self.filtered
            .iter()
            .filter_map(|&ix| commands.get(ix).map(|command| command.name.clone()))
            .collect()
    }

    fn commands(&self) -> &[SlashCommand] {
        self.listing
            .as_ref()
            .and_then(|source| self.cache.get(source))
            .map(|entry| entry.commands.as_slice())
            .unwrap_or_default()
    }

    /// Track the `/` token on every edit or caret move.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let (text, cursor) = {
            let input = self.input.read(cx);
            (input.text().to_owned(), input.cursor_offset())
        };
        let token = slash_token(&text, cursor);
        let still_dismissed = token.as_ref().is_some_and(|token| {
            self.dismissed.as_ref().is_some_and(|(range, value)| {
                token.range == *range && text.get(range.clone()) == Some(value.as_str())
            })
        });
        if still_dismissed || token.is_none() {
            if token.is_none() {
                self.dismissed = None;
            }
            self.close(cx);
            return;
        }
        self.dismissed = None;
        let reopened = self.token.is_none();
        self.token = token;
        if reopened || self.listing != self.source {
            self.open_listing(cx);
        } else {
            self.refilter(cx);
        }
    }

    /// List the current source: from the cache while fresh, else one
    /// `ListCommands`.
    fn open_listing(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        self.listing = self.source.clone();
        let Some(source) = self.source.clone() else {
            self.loading = false;
            self.refilter(cx);
            return;
        };
        let key = source.cache_key();
        let fresh = self.cache.get(&source).is_some_and(|entry| {
            slash_cache_entry_is_fresh(&key, entry.fetched_at, &key, Instant::now())
        });
        if fresh {
            self.loading = false;
            self.refilter(cx);
            return;
        }
        self.request = self.request.wrapping_add(1);
        let request = self.request;
        self.loading = true;
        self.refilter(cx);
        let params = list_commands_params(
            source.harness,
            &source.cwd,
            source.device_id.as_deref(),
            None,
        );
        let task = self
            .state
            .read(cx)
            .work_outline_actions()
            .list_commands(params);
        self._fetch = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if this.request != request {
                    return;
                }
                this.loading = false;
                match result {
                    Ok(commands) => {
                        this.cache.insert(
                            source,
                            SlashCacheEntry {
                                commands,
                                fetched_at: Instant::now(),
                            },
                        );
                    }
                    Err(error) => {
                        tracing::debug!(error = %error.message, "skill discovery failed");
                        this.error = Some(
                            if crate::work_outline::actions::is_transport_failure(&error) {
                                "The project's device is unreachable".into()
                            } else {
                                "Couldn't load skills for this folder".into()
                            },
                        );
                    }
                }
                this.refilter(cx);
            })
            .ok();
        }));
    }

    fn refilter(&mut self, cx: &mut Context<Self>) {
        let query = self
            .token
            .as_ref()
            .map(|token| token.query.clone())
            .unwrap_or_default();
        let skills_only = self.insertion.skills_only();
        let filtered = {
            // Indices into the cached list of the entries this surface offers.
            let offered: Vec<(usize, &str)> = self
                .commands()
                .iter()
                .enumerate()
                .filter(|(_, command)| !(skills_only && is_built_in_command(&command.name)))
                .map(|(ix, command)| (ix, command.name.as_str()))
                .collect();
            let names: Vec<&str> = offered.iter().map(|(_, name)| *name).collect();
            crate::popover::filter_indices(&query, &names)
                .into_iter()
                .map(|ix| offered[ix].0)
                .collect::<Vec<_>>()
        };
        self.filtered = filtered;
        self.active = (!self.filtered.is_empty()).then_some(0);
        self.scroll.set_offset(gpui::Point::default());
        self.sync_input(cx);
        cx.notify();
    }

    fn move_active(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.token.is_none() {
            return;
        }
        self.active = crate::popover::menu_step(self.active, self.filtered.len(), delta);
        if let Some(active) = self.active {
            self.scroll.scroll_to_item(active);
        }
        self.sync_input(cx);
        cx.notify();
    }

    fn accept(&mut self, cx: &mut Context<Self>) {
        let Some(token) = self.token.clone() else {
            return;
        };
        let Some(name) = self
            .active
            .and_then(|active| self.filtered.get(active))
            .and_then(|&ix| self.commands().get(ix))
            .map(|command| command.name.clone())
        else {
            return;
        };
        let text = self.insertion.text(&name);
        // Close first: the replacement's own edit event then finds no token.
        self.close(cx);
        self.input.update(cx, |input, cx| {
            input.replace_plain_token(token.range, &text, cx)
        });
    }

    /// Esc: close and keep this exact token closed until it is edited.
    pub(crate) fn dismiss(&mut self, cx: &mut Context<Self>) {
        let dismissed = self.token.as_ref().and_then(|token| {
            self.input
                .read(cx)
                .text()
                .get(token.range.clone())
                .map(|text| (token.range.clone(), text.to_owned()))
        });
        self.close(cx);
        self.dismissed = dismissed;
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        let was_open = self.token.is_some();
        self.request = self.request.wrapping_add(1);
        self._fetch = None;
        self.token = None;
        self.filtered.clear();
        self.active = None;
        self.loading = false;
        self.error = None;
        self.sync_input(cx);
        if was_open {
            cx.notify();
        }
    }

    /// Hand the input the completion key routing (open / has a selection).
    fn sync_input(&self, cx: &mut Context<Self>) {
        let open = self.token.is_some();
        let has_selection = open && self.active.is_some();
        self.input.update(cx, |input, cx| {
            input.set_mention_controls(open, has_selection, cx)
        });
    }
}

impl Render for SkillCompletion {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = div().absolute().left_0().bottom_0().size_0();
        if self.token.is_none() {
            return root;
        }
        let theme = Theme::of(cx).for_popup();
        let theme = &theme;
        // Match the input frame (the input plus the frames' 6–8px insets).
        let width = self
            .input
            .read(cx)
            .measured_width()
            .map_or(320.0, |width| width + 16.0)
            .max(240.0);
        let commands = self.commands();
        let mut card = crate::popover::popover_card(theme)
            .debug_selector(|| "skill-completion".into())
            .w(px(width))
            .max_h(px(260.0))
            .overflow_hidden()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    // The input blurs on any outside press; keep it focused
                    // so the row click lands with the caret where it was.
                    window.focus(&this.input.focus_handle(cx), cx);
                }),
            )
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.dismiss(cx)));
        let note = |text: SharedString, color| {
            div()
                .px(px(12.0))
                .py(px(10.0))
                .text_size(crate::typography::ui_rems(12.0))
                .text_color(color)
                .child(text)
        };
        if self.loading && commands.is_empty() {
            card = card.child(crate::popover::skeleton_rows(
                "skill-completion-loading",
                theme,
                3,
                cx.entity_id(),
                cx,
            ));
        } else if let Some(error) = self.error.clone() {
            card = card.child(note(error, theme.danger_muted));
        } else if self.filtered.is_empty() {
            card = card.child(note(
                if commands.is_empty()
                    || self
                        .token
                        .as_ref()
                        .is_some_and(|token| token.query.is_empty())
                {
                    "No skills found for this folder".into()
                } else {
                    "No matching skills".into()
                },
                theme.text_muted,
            ));
        } else {
            let entity = cx.entity_id();
            let mut rows = Vec::with_capacity(self.filtered.len());
            for (row_ix, &command_ix) in self.filtered.iter().enumerate() {
                let Some(command) = commands.get(command_ix) else {
                    continue;
                };
                let selected = self.active == Some(row_ix);
                rows.push(
                    crate::popover::menu_row(
                        theme,
                        selected,
                        format!("skill-completion-{entity}-{row_ix}"),
                    )
                    .id(("skill-completion-row", row_ix))
                    .debug_selector(move || format!("skill-completion-row-{row_ix}"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.active = Some(row_ix);
                        this.accept(cx);
                    }))
                    .child(slash_row_body(theme, command))
                    .into_any_element(),
                );
            }
            card = card.child(
                div()
                    .id("skill-completion-list")
                    .max_h(px(252.0))
                    .flex()
                    .flex_col()
                    .gap(px(crate::popover::MENU_GAP))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .children(rows),
            );
        }
        let id = format!("skill-completion-{}", cx.entity_id());
        root.child(if self.over_modal {
            crate::popover::anchored_menu_below_over_modal(id, card.into_any_element())
        } else {
            crate::popover::anchored_menu_below_gap(id, card.into_any_element(), None, 4.0)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_instruction_editors_drop_built_in_commands() {
        assert!(SkillInsertion::InstructionText.skills_only());
        assert!(!SkillInsertion::SlashCommand.skills_only());
        for name in ["compact", "clear", "model", "init", "review"] {
            assert!(is_built_in_command(name), "{name}");
        }
        for name in [
            "code-review",
            "open-pr",
            "simplify",
            "anthropic-skills:docx",
        ] {
            assert!(!is_built_in_command(name), "{name}");
        }
    }

    #[test]
    fn insertion_forms_match_their_surfaces() {
        assert_eq!(
            SkillInsertion::SlashCommand.text("code-review"),
            "/code-review"
        );
        assert_eq!(
            SkillInsertion::InstructionText.text("code-review"),
            "`code-review` skill"
        );
    }
}
