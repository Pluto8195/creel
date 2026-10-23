//! Shell host of the work-context strip (`chat_work_context`): reads the
//! stores, asks for the PR, Linear issue and artifact data the strip shows
//! (the same fetch paths and caches the Outline chips use), and wires the
//! clicks to the Shell. The strip itself renders only what the stores hold.
use super::*;

use std::collections::HashMap;
use std::rc::Rc;

use crate::chat_work_context::{self, ResolveInput, Variant, WorkContext, strip};

/// Height the strip's line adds under the chat header metadata; the
/// transcript's top fade clears it (the expanded panel overlays the
/// transcript on an opaque background).
pub(super) const CHAT_WORK_CONTEXT_CLEARANCE: f32 = 24.0;

impl Shell {
    /// The Chat's work context, when its item, an ancestor, or the review
    /// launcher gives it one. Never fetches; see [`Self::request_work_context_data`].
    fn resolve_chat_work_context(&self, chat_id: &str, cx: &Context<Self>) -> Option<WorkContext> {
        let links = self
            .chat_header_links
            .get(chat_id)
            .and_then(|entry| entry.status.as_ref())
            .map(chat_work_context::chat_prs)
            .unwrap_or_default();
        let review_classified = self
            .chat_header_classification
            .get(chat_id)
            .is_some_and(|entry| entry.category_key.as_deref() == Some("pr_review"));
        let state = self.state.read(cx);
        let chat = state.chats.iter().find(|chat| chat.id == chat_id)?;
        let space_id = chat.space_id.as_deref();
        let snapshot = space_id.and_then(|id| state.work_outline(id).snapshot().cloned());
        let artifacts = space_id.and_then(|id| state.artifacts().snapshot(id).cloned());
        chat_work_context::resolve(&ResolveInput {
            chat_id,
            snapshot: snapshot.as_deref(),
            chat_prs: &links,
            review_classified,
            artifacts: artifacts.as_deref(),
        })
    }

    /// Ask the stores for what `context` shows (cached, so cheap on every
    /// render) and copy it for the strip.
    fn request_work_context_data(
        &mut self,
        context: &WorkContext,
        cx: &mut Context<Self>,
    ) -> strip::StripData {
        let (prs, ticket, project_id) = match &context.variant {
            Variant::PrReview(review) => (review.prs.clone(), review.ticket.clone(), None),
            Variant::Implementation(implementation) => (
                implementation.prs.clone(),
                implementation.ticket.clone(),
                None,
            ),
            Variant::Planning(planning) => (
                Vec::new(),
                None,
                planning.project.as_ref().map(|project| project.id.clone()),
            ),
        };
        for pr in &prs {
            crate::github::store::request_status(&self.state, pr, false, cx);
        }
        if let Some(ticket) = &ticket {
            crate::linear::store::request_issue(&self.state, &ticket.identifier, false, cx);
        }
        let state = self.state.read(cx);
        let entries: HashMap<String, _> = prs
            .iter()
            .filter_map(|pr| Some((pr.key(), state.github.status(pr)?.clone())))
            .collect();
        strip::StripData {
            prs: entries,
            issue: ticket
                .as_ref()
                .and_then(|ticket| state.linear.issue(&ticket.identifier).cloned()),
            project: project_id.and_then(|id| {
                state
                    .linear
                    .projects
                    .as_ref()?
                    .iter()
                    .find(|project| project.id == id)
                    .cloned()
            }),
            now: chrono::Utc::now(),
        }
    }

    /// The work-context strip for the full chat header, with the extra
    /// clearance it needs (0 when the Chat has no context).
    pub(super) fn render_chat_work_context(
        &mut self,
        chat_id: &str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let Some(context) = self.resolve_chat_work_context(chat_id, cx) else {
            self.chat_header_extra_clearance = 0.0;
            return None;
        };
        self.chat_header_extra_clearance = CHAT_WORK_CONTEXT_CLEARANCE;
        let data = self.request_work_context_data(&context, cx);
        let expanded = self.chat_work_context_open.contains(chat_id);
        let space_id = self
            .state
            .read(cx)
            .chats
            .iter()
            .find(|chat| chat.id == chat_id)
            .and_then(|chat| chat.space_id.clone());
        let shell = cx.entity();
        let toggle_chat = chat_id.to_owned();
        let toggle_shell = shell.clone();
        let url_shell = shell.clone();
        let item_shell = shell.clone();
        let artifact_shell = shell.clone();
        let actions = strip::Actions {
            toggle: Rc::new(move |_, cx| {
                toggle_shell.update(cx, |this, cx| {
                    if !this.chat_work_context_open.remove(&toggle_chat) {
                        this.chat_work_context_open.insert(toggle_chat.clone());
                    }
                    cx.notify();
                });
            }),
            open_url: Rc::new(move |url, window, cx| {
                url_shell.update(cx, |this, cx| this.open_work_item_reference(&url, window, cx));
            }),
            open_item: Rc::new(move |item_id, window, cx| {
                let Some(space_id) = space_id.clone() else {
                    return;
                };
                item_shell.update(cx, |this, cx| {
                    this.reveal_work_item_in_outline(&space_id, &item_id, window, cx)
                });
            }),
            open_artifact: Rc::new(move |artifact_id, window, cx| {
                artifact_shell.update(cx, |this, cx| this.open_artifact(artifact_id, window, cx));
            }),
            shell: Some(shell.downgrade()),
        };
        Some(strip::render(&context, &data, expanded, &actions, theme))
    }

    /// Show `item_id` in the Outline (expanding collapsed ancestors).
    fn reveal_work_item_in_outline(
        &mut self,
        space_id: &str,
        item_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_overview(cx);
        let page = self.ensure_overview_page(cx);
        page.update(cx, |overview, cx| overview.focus_outline_on_chat(None, window, cx));
        let key = crate::work_outline::model::row_key::item(space_id, item_id);
        let outline = page.read(cx).outline_entity();
        outline.update(cx, |outline, cx| outline.reveal(&key, cx));
    }
}
