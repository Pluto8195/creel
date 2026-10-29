//! Undo, redo, and "Recent changes" for the Outline, over the action log
//! (`work_outline::action_log`).
//!
//! ⌘Z / ⇧⌘Z (and Ctrl) are bound under the outline key context, which is
//! never active while a text input has focus, so text fields keep their own
//! undo. Undo and redo send the logged inverse (or forward) step through the
//! normal engine operations, after checking the live snapshot still matches
//! what the entry left behind; a changed target stops the run with a message
//! instead of overwriting somebody else's change.

use super::super::action_log::{self, Draft, Recent};
use super::super::actions::WorkOutlineActions;
use super::*;

/// How long to wait for the snapshot to show an applied step before the next
/// step of a multi-step undo (or the next ⌘Z) is allowed anyway.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(2);
const SETTLE_POLL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    Undo,
    Redo,
}

impl Direction {
    fn verb(self) -> &'static str {
        match self {
            Direction::Undo => "undo",
            Direction::Redo => "redo",
        }
    }

    fn past(self) -> &'static str {
        match self {
            Direction::Undo => "Undid",
            Direction::Redo => "Redid",
        }
    }

    fn nothing(self) -> &'static str {
        match self {
            Direction::Undo => "Nothing to undo",
            Direction::Redo => "Nothing to redo",
        }
    }
}

impl OutlineView {
    /// Log a change that was just sent. `draft` is built from the snapshot
    /// before the change (`action_log::*_entry`); `None` records nothing.
    /// Its `operation_id` is the forward request's id while the reply is
    /// pending (see [`Self::confirm_change`] / [`Self::forget_change`]).
    pub(crate) fn record_change(
        &mut self,
        space_id: &str,
        draft: Option<Draft>,
        cx: &mut Context<Self>,
    ) {
        let Some(draft) = draft else {
            return;
        };
        self.state
            .update(cx, |state, _| state.action_log.record(space_id, draft));
        cx.notify();
    }

    /// The log entry for a direct restructuring, read from the snapshot as it
    /// is before the change.
    pub(super) fn direct_draft(
        &self,
        space_id: &str,
        mutation: &DirectMutation,
        operation_id: &str,
    ) -> Option<Draft> {
        let snapshot = self.snapshot(space_id)?;
        let operation_id = Some(operation_id.to_owned());
        let chat_title = |chat_id: &str| {
            self.inputs
                .chats
                .iter()
                .find(|chat| chat.id == chat_id)
                .map(|chat| chat.title.clone())
                .unwrap_or_default()
        };
        match mutation {
            DirectMutation::Move(plan) => action_log::move_entry(
                snapshot,
                &plan.work_item_id,
                plan.destination_parent.as_deref(),
                &plan.placement,
                operation_id,
            ),
            DirectMutation::AssignChat {
                chat_id,
                work_item_id,
            } => Some(action_log::chat_entry(
                snapshot,
                chat_id,
                &chat_title(chat_id),
                Some(work_item_id),
                operation_id,
            )),
            DirectMutation::RemoveChat { chat_id } => Some(action_log::chat_entry(
                snapshot,
                chat_id,
                &chat_title(chat_id),
                None,
                operation_id,
            )),
        }
    }

    /// The forward request was acknowledged.
    pub(crate) fn confirm_change(&mut self, operation_id: &str, cx: &mut Context<Self>) {
        self.state
            .update(cx, |state, _| state.action_log.confirm(operation_id));
    }

    /// The forward request failed: it never happened, so it cannot be undone.
    pub(crate) fn forget_change(&mut self, operation_id: &str, cx: &mut Context<Self>) {
        self.state
            .update(cx, |state, _| state.action_log.fail(operation_id));
    }

    /// Whose log ⌘Z acts on: the selected row's Space when it has history,
    /// otherwise the Space last changed.
    pub(crate) fn undo_space(&self, cx: &App) -> Option<String> {
        let log = &self.state.read(cx).action_log;
        let selected = self
            .selected_row()
            .and_then(|row| row.item().map(|item| item.space_id.clone()))
            .filter(|space| log.len(space) > 0);
        selected.or_else(|| log.last_space().map(str::to_owned))
    }

    /// What ⌘Z would undo.
    pub(crate) fn next_undo_label(&self, cx: &App) -> Option<String> {
        let space = self.undo_space(cx)?;
        let state = self.state.read(cx);
        let entry = state.action_log.next_undo(&space)?;
        Some(entry.label.clone())
    }

    /// What ⇧⌘Z would redo.
    pub(crate) fn next_redo_label(&self, cx: &App) -> Option<String> {
        let space = self.undo_space(cx)?;
        let state = self.state.read(cx);
        let entry = state.action_log.next_redo(&space)?;
        Some(entry.label.clone())
    }

    pub(crate) fn undo_change(&mut self, cx: &mut Context<Self>) {
        self.run_log(Direction::Undo, 1, cx);
    }

    pub(crate) fn redo_change(&mut self, cx: &mut Context<Self>) {
        self.run_log(Direction::Redo, 1, cx);
    }

    /// The recent changes of the current Space, newest first.
    pub(crate) fn recent_changes(&self, cx: &App) -> Vec<Recent> {
        self.undo_space(cx)
            .map(|space| {
                self.state
                    .read(cx)
                    .action_log
                    .recent(&space, action_log::RECENT_LIMIT)
            })
            .unwrap_or_default()
    }

    /// Undo back to and including `entry_id`, newest first.
    pub(crate) fn undo_back_to(&mut self, entry_id: u64, cx: &mut Context<Self>) {
        let Some(space) = self.undo_space(cx) else {
            return;
        };
        let count = self
            .state
            .read(cx)
            .action_log
            .undo_chain(&space, entry_id)
            .map(|chain| chain.len());
        match count {
            Some(count) => self.run_log(Direction::Undo, count, cx),
            None => self.show_notice("That change is not on the undo stack", cx),
        }
    }

    /// "Recent changes…": the log as a picker. Choosing an applied row
    /// undoes back to it; choosing an undone row redoes up to it.
    pub(crate) fn open_recent_changes(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), &'static str> {
        let recent = self.recent_changes(cx);
        if recent.is_empty() {
            return Err("No changes to undo yet");
        }
        let entries = recent
            .into_iter()
            .map(|recent| PickerEntry {
                label: if recent.undone {
                    format!("{} (undone)", recent.label)
                } else {
                    recent.label
                },
                depth: 0,
                value: Some(format!(
                    "{}{}",
                    if recent.undone { 'r' } else { 'u' },
                    recent.id
                )),
            })
            .collect();
        self.open_picker(
            "Recent changes: undo back to\u{2026}".to_string(),
            entries,
            PickerPurpose::RecentChanges,
            window,
            cx,
        );
        Ok(())
    }

    /// A row of the Recent changes picker was chosen.
    pub(super) fn choose_recent_change(
        &mut self,
        entry: &PickerEntry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_picker(window, cx);
        let value = entry.value.as_deref().unwrap_or_default();
        let Some(id) = value.get(1..).and_then(|id| id.parse::<u64>().ok()) else {
            return;
        };
        if value.starts_with('u') {
            self.undo_back_to(id, cx);
            return;
        }
        let Some(space) = self.undo_space(cx) else {
            return;
        };
        let steps = {
            let recent = self.state.read(cx).action_log.recent(&space, usize::MAX);
            // Undone rows are the newest; redo runs oldest-undone first.
            recent
                .iter()
                .rev()
                .filter(|r| r.undone)
                .position(|r| r.id == id)
                .map(|ix| ix + 1)
        };
        match steps {
            Some(count) => self.run_log(Direction::Redo, count, cx),
            None => self.show_notice("That change is no longer in the log", cx),
        }
    }

    /// Check and send the next `count` steps one after another, stopping at
    /// the first conflict or failure.
    fn run_log(&mut self, direction: Direction, count: usize, cx: &mut Context<Self>) {
        const BUSY: &str = "Still applying the last change. Try again in a moment";
        if self.log_busy {
            self.show_notice(BUSY, cx);
            return;
        }
        let Some(space) = self.undo_space(cx) else {
            self.show_notice(direction.nothing(), cx);
            return;
        };
        if self.restructuring_busy() || !self.optimistic.is_empty() {
            self.show_notice(BUSY, cx);
            return;
        }
        self.log_busy = true;
        cx.spawn(async move |this, cx| {
            let mut done: Vec<String> = Vec::new();
            for _ in 0..count {
                let started = this
                    .update(cx, |this, cx| this.start_log_step(&space, direction, cx))
                    .ok()
                    .flatten();
                let Some((steps, actions, entry)) = started else {
                    break;
                };
                // A batch (Group into…) is several requests, sent in order.
                let mut outcome = Ok(());
                for step in steps {
                    if let Err(error) = step.run(&actions).await {
                        outcome = Err(error);
                        break;
                    }
                }
                match outcome {
                    Ok(()) => {
                        done.push(entry.label.clone());
                        // Say so now; only the next step waits for the frame.
                        if let Some(summary) = summarize(direction, &done) {
                            this.update(cx, |this, cx| {
                                this.dismiss_move_toast(cx);
                                this.show_notice(summary, cx);
                            })
                            .ok();
                        }
                        // Wait for the snapshot to show the step so the next
                        // check (and the next ⌘Z) sees it.
                        let expect = match direction {
                            Direction::Undo => entry.before.clone(),
                            Direction::Redo => entry.after.clone(),
                        };
                        let mut waited = Duration::ZERO;
                        while waited < SETTLE_TIMEOUT {
                            let shown = this
                                .update(cx, |this, _| {
                                    this.snapshot(&space)
                                        .is_some_and(|snapshot| expect.check(snapshot).is_ok())
                                })
                                .unwrap_or(true);
                            if shown {
                                break;
                            }
                            cx.background_executor().timer(SETTLE_POLL).await;
                            waited += SETTLE_POLL;
                        }
                    }
                    Err(error) => {
                        this.update(cx, |this, cx| {
                            // It did not happen: put the cursor back.
                            this.state.update(cx, |state, _| match direction {
                                Direction::Undo => state.action_log.mark_redone(&space, entry.id),
                                Direction::Redo => state.action_log.mark_undone(&space, entry.id),
                            });
                            this.show_error(error.message.clone(), cx);
                        })
                        .ok();
                        break;
                    }
                }
            }
            this.update(cx, |this, cx| {
                this.log_busy = false;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Check the next entry against the live snapshot, move the log cursor,
    /// and send its step. `None` after telling the user why not.
    fn start_log_step(
        &mut self,
        space: &str,
        direction: Direction,
        cx: &mut Context<Self>,
    ) -> Option<(Vec<action_log::Step>, WorkOutlineActions, action_log::Entry)> {
        let entry = {
            let log = &self.state.read(cx).action_log;
            match direction {
                Direction::Undo => log.next_undo(space),
                Direction::Redo => log.next_redo(space),
            }
            .cloned()
        };
        let Some(entry) = entry else {
            self.show_notice(direction.nothing(), cx);
            return None;
        };
        let (probe, step) = match direction {
            Direction::Undo => (&entry.after, &entry.inverse),
            Direction::Redo => (&entry.before, &entry.forward),
        };
        let Some(snapshot) = self.snapshot(space) else {
            self.show_notice("This project's outline is not loaded", cx);
            return None;
        };
        if let Err(why) = probe.check(snapshot) {
            self.show_notice(
                format!(
                    "Can\u{2019}t {} \u{201c}{}\u{201d}: {}. Nothing was changed",
                    direction.verb(),
                    entry.label,
                    why.to_lowercase()
                ),
                cx,
            );
            return None;
        }
        let actions = self.state.read(cx).work_outline_actions();
        let steps = step.leaves();
        self.state.update(cx, |state, _| match direction {
            Direction::Undo => state.action_log.mark_undone(space, entry.id),
            Direction::Redo => state.action_log.mark_redone(space, entry.id),
        });
        Some((steps, actions, entry))
    }
}

fn summarize(direction: Direction, done: &[String]) -> Option<String> {
    match done {
        [] => None,
        [only] => Some(format!("{}: {only}", direction.past())),
        [.., last] => Some(format!(
            "{} {} changes, back to: {last}",
            direction.past(),
            done.len()
        )),
    }
}

#[cfg(test)]
mod summary_tests {
    use super::*;

    #[test]
    fn summaries_name_the_change_or_the_count() {
        assert_eq!(summarize(Direction::Undo, &[]), None);
        assert_eq!(
            summarize(Direction::Undo, &["moved \u{2018}X\u{2019}".into()]).as_deref(),
            Some("Undid: moved \u{2018}X\u{2019}")
        );
        assert_eq!(
            summarize(Direction::Redo, &["a".into(), "b".into()]).as_deref(),
            Some("Redid 2 changes, back to: b")
        );
    }
}
