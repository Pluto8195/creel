//! The non-blocking notice for a move that changes assigned Chats'
//! effective instructions: "Moved X · instructions changed for N chats (they
//! keep their current instructions)" with **Undo**.
//!
//! Work-item moves (keys, drag and drop, menu, Move To… picker) and Chat
//! moves / associations apply at once; this replaces the blocking drift
//! confirmation for them. Rapid moves coalesce into one toast whose counts
//! grow; Undo reverts the most recent move only, by sending the inverse
//! mutation captured before it. The other drift triggers (inheritance,
//! instruction edits, execution configuration, Apply) keep their modal.

use std::collections::BTreeSet;

use super::*;

/// How long the toast stays before it dismisses itself.
pub(crate) const MOVE_TOAST_LIFETIME: Duration = Duration::from_secs(6);

/// A move's toast, captured before the move is sent.
pub(super) struct MoveToastDraft {
    space_id: String,
    title: String,
    chat_ids: Vec<String>,
    undo: Option<DirectMutation>,
}

#[derive(Debug, Clone)]
pub(super) struct MoveToast {
    space_id: String,
    /// Moves coalesced into this toast.
    moves: usize,
    /// The most recent move's title.
    title: String,
    /// Every Chat whose instructions changed across the coalesced moves.
    chat_ids: BTreeSet<String>,
    /// Reverts the most recent move.
    undo: Option<DirectMutation>,
    /// The most recent move's operation id (a failure dismisses the toast).
    operation_id: String,
    seq: u64,
}

impl MoveToast {
    fn text(&self) -> String {
        let moved = if self.moves > 1 {
            format!("Moved {} and {} more", self.title, self.moves - 1)
        } else {
            format!("Moved {}", self.title)
        };
        let chats = self.chat_ids.len();
        let (count, keep) = if chats == 1 {
            ("1 chat".to_string(), "it keeps its current instructions")
        } else {
            (
                format!("{chats} chats"),
                "they keep their current instructions",
            )
        };
        format!("{moved} \u{b7} instructions changed for {count} ({keep})")
    }
}

impl OutlineView {
    /// Capture what the toast and its Undo need, before `mutation` is shown.
    pub(super) fn draft_move_toast(
        &self,
        space_id: &str,
        mutation: &DirectMutation,
        impact: &instructions::DriftImpact,
    ) -> MoveToastDraft {
        let snapshot = self.snapshot(space_id);
        let untitled = |title: &str| {
            if title.trim().is_empty() {
                "Untitled".to_string()
            } else {
                title.to_string()
            }
        };
        let chat_title = |chat_id: &str| {
            self.inputs
                .chats
                .iter()
                .find(|chat| chat.id == chat_id)
                .map(|chat| untitled(&chat.title))
                .unwrap_or_else(|| "Chat".to_string())
        };
        let (title, undo) = match mutation {
            DirectMutation::Move(plan) => {
                let item = snapshot.and_then(|snapshot| snapshot.item(&plan.work_item_id));
                let title = item.map_or_else(|| "item".to_string(), |item| untitled(&item.title));
                let undo = snapshot.zip(item).map(|(snapshot, item)| {
                    let parent = item.parent_work_item_id.clone();
                    let siblings = restructure::live_children(snapshot, parent.as_deref());
                    let previous = siblings
                        .iter()
                        .position(|sibling| sibling.id == item.id)
                        .and_then(|ix| ix.checked_sub(1))
                        .map(|ix| siblings[ix].id.clone());
                    DirectMutation::Move(MovePlan {
                        work_item_id: item.id.clone(),
                        destination_parent: parent,
                        placement: match previous {
                            Some(sibling_work_item_id) => WorkOutlinePlacement::After {
                                sibling_work_item_id,
                            },
                            None => WorkOutlinePlacement::First,
                        },
                    })
                });
                (title, undo)
            }
            DirectMutation::AssignChat { chat_id, .. } | DirectMutation::RemoveChat { chat_id } => {
                let previous = snapshot
                    .and_then(|snapshot| instructions::assigned_chats(snapshot).remove(chat_id));
                let undo = match previous {
                    Some(work_item_id) => DirectMutation::AssignChat {
                        chat_id: chat_id.clone(),
                        work_item_id,
                    },
                    None => DirectMutation::RemoveChat {
                        chat_id: chat_id.clone(),
                    },
                };
                (chat_title(chat_id), Some(undo))
            }
        };
        MoveToastDraft {
            space_id: space_id.to_owned(),
            title,
            chat_ids: impact.chat_ids.clone(),
            undo,
        }
    }

    /// Show (or update) the toast for the move sent as `operation_id`. A move
    /// that did not go out shows nothing.
    pub(super) fn show_move_toast(
        &mut self,
        draft: MoveToastDraft,
        operation_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(operation_id) = operation_id.filter(|id| {
            self.optimistic
                .iter()
                .any(|entry| &entry.operation_id == id)
        }) else {
            return;
        };
        self.move_toast_seq = self.move_toast_seq.wrapping_add(1);
        let seq = self.move_toast_seq;
        let mut toast = match self.move_toast.take() {
            Some(mut toast) if toast.space_id == draft.space_id => {
                toast.moves += 1;
                toast
            }
            _ => MoveToast {
                space_id: draft.space_id.clone(),
                moves: 1,
                title: String::new(),
                chat_ids: BTreeSet::new(),
                undo: None,
                operation_id: String::new(),
                seq,
            },
        };
        toast.title = draft.title;
        toast.chat_ids.extend(draft.chat_ids);
        toast.undo = draft.undo;
        toast.operation_id = operation_id;
        toast.seq = seq;
        self.move_toast = Some(toast);
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(MOVE_TOAST_LIFETIME).await;
            this.update(cx, |this, cx| {
                if this
                    .move_toast
                    .as_ref()
                    .is_some_and(|toast| toast.seq == seq)
                {
                    this.move_toast = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn move_toast_text(&self) -> Option<String> {
        self.move_toast.as_ref().map(MoveToast::text)
    }

    pub(crate) fn dismiss_move_toast(&mut self, cx: &mut Context<Self>) {
        if self.move_toast.take().is_some() {
            cx.notify();
        }
    }

    /// The move sent as `operation_id` failed: its toast no longer applies.
    pub(super) fn dismiss_move_toast_for(&mut self, operation_id: &str, cx: &mut Context<Self>) {
        if self
            .move_toast
            .as_ref()
            .is_some_and(|toast| toast.operation_id == operation_id)
        {
            self.dismiss_move_toast(cx);
        }
    }

    /// Undo: send the inverse of the most recent move under a new operation
    /// id. It applies at once and raises no toast or confirmation of its own.
    pub(crate) fn undo_move(&mut self, cx: &mut Context<Self>) {
        let Some(toast) = self.move_toast.take() else {
            return;
        };
        let Some(undo) = toast.undo.clone() else {
            cx.notify();
            return;
        };
        if self.restructuring_busy() {
            // Still applying the move: keep the toast so Undo can be retried.
            self.move_toast = Some(toast);
            self.show_notice("Still applying the move. Try Undo again in a moment", cx);
            return;
        }
        // A retry of a failed attempt would replay its operation id.
        self.attempt = None;
        // The move is in the action log as its newest entry: it is undone now.
        self.state.update(cx, |state, _| {
            let newest = state
                .action_log
                .next_undo(&toast.space_id)
                .filter(|entry| {
                    matches!(
                        entry.forward,
                        super::super::action_log::Step::Move { .. }
                            | super::super::action_log::Step::Assign { .. }
                            | super::super::action_log::Step::RemoveChat { .. }
                    )
                })
                .map(|entry| entry.id);
            if let Some(id) = newest {
                state.action_log.mark_undone(&toast.space_id, id);
            }
        });
        self.skip_log = true;
        self.send_direct(toast.space_id, undo, cx);
        self.skip_log = false;
        cx.notify();
    }

    pub(super) fn render_move_toast(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let text = self.move_toast_text()?;
        let has_undo = self
            .move_toast
            .as_ref()
            .is_some_and(|toast| toast.undo.is_some());
        let button = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .flex_none()
                .px(px(6.0))
                .rounded(px(4.0))
                .cursor_pointer()
                .hover(|el| el.bg(theme.element_hover))
                .child(label)
        };
        Some(
            div()
                .debug_selector(|| "outline-move-toast".into())
                .flex_none()
                .flex()
                .items_center()
                .gap(px(8.0))
                .mx(px(10.0))
                .mb(px(4.0))
                .px(px(8.0))
                .py(px(4.0))
                .rounded(px(6.0))
                .bg(theme.surface_card)
                .border_1()
                .border_color(theme.border)
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(theme.text)
                .child(div().flex_1().min_w_0().child(SharedString::from(text)))
                .when(has_undo, |el| {
                    el.child(
                        button("outline-move-toast-undo", "Undo")
                            .on_click(cx.listener(|this, _, _, cx| this.undo_move(cx))),
                    )
                })
                .child(
                    button("outline-move-toast-close", "\u{2715}")
                        .on_click(cx.listener(|this, _, _, cx| this.dismiss_move_toast(cx))),
                )
                .into_any_element(),
        )
    }
}
