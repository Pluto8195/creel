//! Linear in the Outline tree (Linear-enabled Spaces only): the live issue
//! chip (state color + name, priority, assignee; click opens the issue per
//! the web-link preference) and the project chip on linked items (with
//! "Fix link" when a pasted-URL link stores the slug), and the
//! drop of a Linear panel issue onto a work item row (same indicator as an
//! Item drop's "into"; creates the work item under that row).
//!
//! A child module of `outline_view` so it reads the Outline's snapshots
//! without widening that view's API.
use gpui::{AnyElement, Context, SharedString, WeakEntity, div, prelude::*, px};
use zeron_proto::linear::LinearIssueSummary;

use super::super::chips::TextTooltip;
use super::super::model::{ItemRow, row_key};
use super::{OutlineEvent, OutlineView};
use crate::linear::project_links;
use crate::linear::store::{self, IssueEntry};
use crate::theme::Theme;

/// The drag payload of a Linear panel issue row
/// (`OutlineDrag::LinearIssue`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LinearIssueDrag {
    /// The panel's Space: only its items accept the drop.
    pub space_id: String,
    pub issue: Box<LinearIssueSummary>,
}

/// One optional chip after another (the item row takes one slot).
pub(crate) fn join_chips(a: Option<AnyElement>, b: Option<AnyElement>) -> Option<AnyElement> {
    match (a, b) {
        (Some(a), Some(b)) => Some(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(a)
                .child(b)
                .into_any_element(),
        ),
        (a, b) => a.or(b),
    }
}

impl OutlineView {
    /// Whether dropping `drag` on item `item_id` of `space_id` creates a
    /// work item there: same Space, Linear on, live target, nothing being
    /// edited.
    pub(super) fn accepts_linear_drop(
        &self,
        drag: &LinearIssueDrag,
        space_id: &str,
        item_id: &str,
    ) -> bool {
        if self.is_editing() || drag.space_id != space_id {
            return false;
        }
        let Some(snapshot) = self.snapshot(space_id) else {
            return false;
        };
        crate::linear::space_enabled(Some(snapshot))
            && snapshot
                .item(item_id)
                .is_some_and(|item| item.archived_at.is_none())
    }

    /// Hand an accepted drop to the Overview (the Linear panel creates it).
    pub(super) fn drop_linear_issue(
        &mut self,
        drag: &LinearIssueDrag,
        target: (&str, &str),
        cx: &mut Context<Self>,
    ) {
        if !self.accepts_linear_drop(drag, target.0, target.1) {
            return;
        }
        cx.emit(OutlineEvent::LinearIssueDropped {
            space_id: target.0.to_string(),
            item_id: target.1.to_string(),
            issue: drag.issue.clone(),
        });
    }

    /// Select and reveal `key` now, or as soon as a refresh brings it in
    /// (a just-created item), expanding `parent_key` so it shows.
    pub(crate) fn reveal_when_shown(
        &mut self,
        key: &str,
        parent_key: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        if let Some(parent) = parent_key {
            self.reveal(parent, cx);
            self.set_collapsed(parent, false, cx);
        }
        if !self.reveal(key, cx) {
            self.pending_select = Some(key.to_string());
        }
        cx.notify();
    }

    /// The Linear chips of an item row (none outside Linear-enabled Spaces).
    pub(super) fn linear_chips(
        &self,
        key: &str,
        item: &ItemRow,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let snapshot = self.snapshot(&item.space_id)?;
        if !crate::linear::space_enabled(Some(snapshot)) {
            return None;
        }
        let issue = snapshot
            .linear_issue_reference(&item.item_id)
            .map(|reference| (reference.external_id.clone(), reference.url.clone()));
        let project_reference = snapshot.linear_project_reference(&item.item_id).cloned();
        let project = project_reference.as_ref().map(|reference| {
            (
                reference
                    .cached_title
                    .clone()
                    .unwrap_or_else(|| "Linear project".into()),
                reference.url.clone(),
            )
        });
        // A pasted-URL link (slug, not id) that My projects resolves.
        let repair = project_reference.and_then(|reference| {
            let projects = self.state.read(cx).linear.projects.clone()?;
            let project = project_links::repair_target(&reference, &projects)?.clone();
            Some((reference, project))
        });
        let shell = self.shell.clone();
        let issue_chip = issue.map(|(identifier, url)| {
            store::request_issue(&self.state, &identifier, false, cx);
            let entry = self.state.read(cx).linear.issue(&identifier).cloned();
            issue_chip(
                format!("outline-linear-{key}"),
                &identifier,
                entry.as_ref(),
                url,
                shell.clone(),
                theme,
            )
        });
        let project_chip = project.map(|(name, url)| {
            project_chip(
                format!("outline-linear-project-{key}"),
                name,
                url,
                shell,
                theme,
            )
        });
        let fix_chip = repair.map(|(reference, project)| {
            fix_link_chip(format!("outline-linear-fix-{key}"), &project.name, theme)
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.fix_project_link(reference.clone(), project.clone(), cx)
                }))
                .into_any_element()
        });
        join_chips(join_chips(issue_chip, project_chip), fix_chip)
    }

    /// Fix link on a slug-keyed project link: relink by the real project id,
    /// then drop the old reference (`project_links::fix_project_link`).
    pub(super) fn fix_project_link(
        &mut self,
        reference: zeron_proto::WorkItemExternalReferenceView,
        project: zeron_proto::linear::LinearProjectSummary,
        cx: &mut Context<Self>,
    ) {
        let task = project_links::fix_project_link(&self.state, &reference, &project, cx);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| match result {
                Ok(()) => this.show_notice(format!("Fixed the link to {}", project.name), cx),
                Err(error) => this.show_notice(
                    format!("Couldn\u{2019}t fix the link: {}", error.user_text()),
                    cx,
                ),
            })
            .ok();
        })
        .detach();
    }
}

/// "Fix link" beside a project chip whose link stores the URL slug instead
/// of the project id (pasted before My projects knew it). The caller adds
/// the click.
pub(crate) fn fix_link_chip(id: String, project: &str, theme: &Theme) -> gpui::Stateful<gpui::Div> {
    let selector = id.clone();
    let tip: SharedString = format!(
        "This link to {project} was pasted as a URL. Fix link relinks it by the project\u{2019}s \
         id so its issues land here"
    )
    .into();
    div()
        .id(SharedString::from(id))
        .debug_selector(move || selector.clone())
        .flex_none()
        .px(px(4.0))
        .rounded(px(3.0))
        .border_1()
        .border_color(theme.warning.opacity(0.6))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(10.0))
        .text_color(theme.warning)
        .hover(|el| el.bg(theme.element_hover))
        .child("Fix link")
        .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into())
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

fn open_link(
    shell: Option<WeakEntity<crate::shell::Shell>>,
    url: String,
) -> impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static {
    move |_, window, cx| {
        cx.stop_propagation();
        let opened = shell
            .as_ref()
            .and_then(WeakEntity::upgrade)
            .map(|shell| {
                shell.update(cx, |shell, cx| {
                    shell.open_work_item_reference(&url, window, cx)
                })
            })
            .is_some();
        if !opened {
            cx.open_url(&url);
        }
    }
}

/// The live issue chip: state dot + name, priority, assignee.
pub(crate) fn issue_chip(
    id: String,
    identifier: &str,
    entry: Option<&IssueEntry>,
    url: String,
    shell: Option<WeakEntity<crate::shell::Shell>>,
    theme: &Theme,
) -> AnyElement {
    let selector = id.clone();
    let (dot, text, tip): (gpui::Hsla, String, String) = match entry {
        Some(IssueEntry::Ready { issue, .. }) => {
            let mut parts = vec![issue.state.name.clone()];
            parts.extend(crate::linear::priority_text(
                issue.priority,
                &issue.priority_label,
            ));
            parts.extend(issue.assignee.as_ref().map(crate::linear::user_name));
            (
                crate::linear::hex_color(&issue.state.color, theme.text_muted),
                parts.join(" \u{00b7} "),
                format!("{} \u{00b7} {}", issue.identifier, issue.title),
            )
        }
        Some(IssueEntry::Failed { error, .. }) => (
            theme.warning,
            identifier.to_string(),
            format!("{identifier}: {}", error.user_text()),
        ),
        _ => (
            theme.text_faint,
            identifier.to_string(),
            format!("{identifier} (loading from Linear)"),
        ),
    };
    let tip: SharedString = format!("{tip}\nClick to open in Linear").into();
    div()
        .id(SharedString::from(id))
        .debug_selector(move || selector.clone())
        .flex_none()
        .max_w(px(180.0))
        .flex()
        .items_center()
        .gap(px(4.0))
        .px(px(4.0))
        .rounded(px(3.0))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(10.0))
        .text_color(theme.text_muted)
        .hover(|el| el.bg(theme.element_hover).text_color(theme.text))
        .child(div().flex_none().size(px(6.0)).rounded_full().bg(dot))
        .child(div().min_w_0().truncate().child(SharedString::from(text)))
        .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into())
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(open_link(shell, url))
        .into_any_element()
}

/// The project chip of a project-linked item.
pub(crate) fn project_chip(
    id: String,
    name: String,
    url: String,
    shell: Option<WeakEntity<crate::shell::Shell>>,
    theme: &Theme,
) -> AnyElement {
    let selector = id.clone();
    let tip: SharedString = format!("Linear project: {name}\nClick to open in Linear").into();
    div()
        .id(SharedString::from(id))
        .debug_selector(move || selector.clone())
        .flex_none()
        .max_w(px(140.0))
        .flex()
        .items_center()
        .gap(px(3.0))
        .px(px(4.0))
        .rounded(px(3.0))
        .cursor_pointer()
        .text_size(crate::typography::ui_rems(10.0))
        .text_color(theme.text_muted)
        .hover(|el| el.bg(theme.element_hover).text_color(theme.text))
        .child(
            crate::icons::icon(crate::icons::CHECKLIST)
                .size(px(10.0))
                .text_color(theme.text_faint),
        )
        .child(div().min_w_0().truncate().child(SharedString::from(name)))
        .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into())
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(open_link(shell, url))
        .into_any_element()
}

/// Row key of item `item_id` in `space_id` (re-exported for the panel host).
pub(crate) fn item_key(space_id: &str, item_id: &str) -> String {
    row_key::item(space_id, item_id)
}
