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
use zeron_proto::linear::{LinearIssueSummary, LinearStateType};

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

/// A linked issue and what is cached of it.
pub(super) struct LinearIssueRow {
    pub identifier: String,
    pub url: String,
    pub entry: Option<IssueEntry>,
}

/// What an item row shows of its Linear links.
#[derive(Default)]
pub(super) struct LinearParts {
    /// The project chip and "Fix link".
    pub chips: Option<AnyElement>,
    /// The linked issue, drawn as the row's icon cluster.
    pub issue: Option<LinearIssueRow>,
    pub has_project_chip: bool,
    pub has_fix_chip: bool,
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

    /// The Linear parts of an item row (none outside Linear-enabled Spaces):
    /// the project and "Fix link" chips, and the linked issue's data for the
    /// row's icon cluster.
    pub(super) fn linear_parts(
        &self,
        key: &str,
        item: &ItemRow,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> LinearParts {
        let Some(snapshot) = self.snapshot(&item.space_id) else {
            return LinearParts::default();
        };
        if !crate::linear::space_enabled(Some(snapshot)) {
            return LinearParts::default();
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
        let issue = issue.map(|(identifier, url)| {
            store::request_issue(&self.state, &identifier, false, cx);
            let entry = self.state.read(cx).linear.issue(&identifier).cloned();
            LinearIssueRow {
                identifier,
                url,
                entry,
            }
        });
        let has_project_chip = project.is_some();
        let has_fix_chip = repair.is_some();
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
        LinearParts {
            chips: join_chips(project_chip, fix_chip),
            issue,
            has_project_chip,
            has_fix_chip,
        }
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
                Err(error) => this.show_error(
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

/// The issue chip's text: `In Progress · High · Ada`.
pub(crate) fn issue_chip_text(issue: &zeron_proto::linear::LinearIssueSummary) -> String {
    let mut parts = vec![issue.state.name.clone()];
    parts.extend(crate::linear::priority_text(
        issue.priority,
        &issue.priority_label,
    ));
    parts.extend(issue.assignee.as_ref().map(crate::linear::user_name));
    parts.join(" \u{00b7} ")
}

/// The status icon of a Linear workflow state type (drawn next to the state
/// dot on an Outline row).
pub(crate) fn linear_status_icon(state_type: LinearStateType) -> &'static str {
    match state_type {
        LinearStateType::Completed => crate::icons::CHECK,
        LinearStateType::Canceled => crate::icons::CLOSE_CIRCLE,
        LinearStateType::Started => crate::icons::CLOCK_CIRCLE,
        LinearStateType::Triage
        | LinearStateType::Backlog
        | LinearStateType::Unstarted
        | LinearStateType::Unknown => crate::icons::CHECKLIST,
    }
}

/// An Outline row's Linear tooltip: identifier, state, priority and the
/// assignee, then the title.
pub(crate) fn issue_row_tooltip(issue: &LinearIssueSummary) -> String {
    let mut parts = vec![issue.identifier.clone(), issue.state.name.clone()];
    parts.extend(crate::linear::priority_text(
        issue.priority,
        &issue.priority_label,
    ));
    parts.push(match &issue.assignee {
        Some(user) => format!("Assigned to {}", crate::linear::user_name(user)),
        None => "Unassigned".to_string(),
    });
    format!("{}\n{}", parts.join(" \u{00b7} "), issue.title)
}

/// The Linear status slot of an Outline row: the state's dot (when
/// `show_dot`) and status icon in one fixed-width element (identifier,
/// assignee and the rest in the tooltip). Click opens the issue per the
/// web-link preference. The details panel keeps the full text chip
/// ([`issue_chip`]).
pub(crate) fn issue_row_icons(
    id: String,
    identifier: &str,
    entry: Option<&IssueEntry>,
    url: String,
    shell: Option<WeakEntity<crate::shell::Shell>>,
    show_dot: bool,
    theme: &Theme,
) -> AnyElement {
    let selector = id.clone();
    let (color, icon, tip): (gpui::Hsla, Option<&'static str>, String) = match entry {
        Some(IssueEntry::Ready { issue, .. }) => (
            crate::linear::hex_color(&issue.state.color, theme.text_muted),
            Some(linear_status_icon(issue.state.state_type)),
            issue_row_tooltip(issue),
        ),
        Some(IssueEntry::Failed { error, .. }) => (
            theme.warning,
            None,
            format!("{identifier}: {}", error.user_text()),
        ),
        _ => (
            theme.text_faint,
            None,
            format!("{identifier} (loading from Linear)"),
        ),
    };
    let tip: SharedString = format!("{tip}\nClick to open in Linear").into();
    div()
        .id(SharedString::from(id))
        .debug_selector(move || selector.clone())
        .flex_none()
        .w(px(status_width(show_dot)))
        .flex()
        .items_center()
        .justify_between()
        .px(px(3.0))
        .rounded(px(3.0))
        .cursor_pointer()
        .hover(|el| el.bg(theme.element_hover))
        .when(show_dot, |el| {
            el.child(div().flex_none().size(px(6.0)).rounded_full().bg(color))
        })
        .child(
            div()
                .flex_none()
                .size(px(11.0))
                .flex()
                .items_center()
                .justify_center()
                .children(icon.map(|path| {
                    crate::icons::icon(path)
                        .size(px(11.0))
                        .text_color(color)
                })),
        )
        .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into())
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(open_link(shell, url))
        .into_any_element()
}

/// Fixed width of the status slot (dot, gap, icon, padding; just the icon
/// and padding when the dot is dropped) so the slots line up across rows.
fn status_width(show_dot: bool) -> f32 {
    if show_dot {
        STATUS_WIDTH
    } else {
        STATUS_ICON_WIDTH
    }
}

const STATUS_WIDTH: f32 = 28.0;
const STATUS_ICON_WIDTH: f32 = 17.0;
const ASSIGNEE_WIDTH: f32 = 58.0;
const NEEDS_WIDTH: f32 = 16.0;
const SLOT_GAP: f32 = 4.0;

/// Which droppable slots of a Linear row cluster are shown (the needs slot
/// and the status icon never drop).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LinearSlots {
    pub assignee: bool,
    pub dot: bool,
}

impl LinearSlots {
    pub(crate) const ALL: Self = Self {
        assignee: true,
        dot: true,
    };

    /// The width of the cluster with these slots.
    pub(crate) fn width(self) -> f32 {
        let mut widths = vec![NEEDS_WIDTH, status_width(self.dot)];
        if self.assignee {
            widths.push(ASSIGNEE_WIDTH);
        }
        widths.iter().sum::<f32>() + SLOT_GAP * (widths.len() as f32 - 1.0)
    }
}

/// The slots that fit in `room` pixels: the state dot drops first (the icon
/// carries the state's color), then the assignee.
pub(crate) fn linear_visible_slots(room: f32) -> LinearSlots {
    let mut slots = LinearSlots::ALL;
    if slots.width() > room {
        slots.dot = false;
    }
    if slots.width() > room {
        slots.assignee = false;
    }
    slots
}

/// Everything the Linear cluster needs from the row.
pub(crate) struct LinearRowArgs<'a> {
    pub key: &'a str,
    pub issue: &'a LinearIssueRow,
    pub shell: Option<WeakEntity<crate::shell::Shell>>,
    /// The item's attention indicator, when it needs the user.
    pub attention: Option<AnyElement>,
    pub chat_count: u32,
    /// "Including descendants: …" (the tooltip's last line).
    pub totals_line: Option<String>,
    pub visible: LinearSlots,
}

/// The cluster's tooltip: the issue line, then the chat count and totals the
/// row no longer shows as text.
pub(crate) fn linear_group_line(
    identifier: &str,
    entry: Option<&IssueEntry>,
    chat_count: u32,
    totals_line: Option<&str>,
) -> String {
    let mut line = match entry {
        Some(IssueEntry::Ready { issue, .. }) => issue_row_tooltip(issue),
        Some(IssueEntry::Failed { error, .. }) => format!("{identifier}: {}", error.user_text()),
        _ => format!("{identifier} (loading from Linear)"),
    };
    if chat_count > 0 {
        line = format!(
            "{line}\n{chat_count} associated chat{}",
            if chat_count == 1 { "" } else { "s" }
        );
    }
    if let Some(totals) = totals_line {
        line = format!("{line}\n{totals}");
    }
    line
}

/// The icon cluster of a Linear row, like a PR row's: `assignee · needs ·
/// status`. The totals text, chat count and references icon are folded into
/// the tooltip. Without a fetched issue the status slot carries the row and
/// the assignee stays empty so the columns still line up.
pub(crate) fn issue_row_slots(args: LinearRowArgs<'_>, theme: &Theme) -> AnyElement {
    let LinearRowArgs {
        key,
        issue,
        shell,
        attention,
        chat_count,
        totals_line,
        visible,
    } = args;
    let ready = match &issue.entry {
        Some(IssueEntry::Ready { issue, .. }) => Some(issue),
        _ => None,
    };
    let line: SharedString = linear_group_line(
        &issue.identifier,
        issue.entry.as_ref(),
        chat_count,
        totals_line.as_deref(),
    )
    .into();
    let assignee = ready.and_then(|ready| ready.assignee.as_ref()).and_then(|user| {
        let name = crate::linear::user_name(user);
        crate::github::row_icons::author_text(&name, None).map(|text| (text, name))
    });
    let assignee = assignee.map(|(text, name)| {
        let selector = format!("outline-linear-assignee-{key}");
        let tip: SharedString = format!("Assigned to {name}").into();
        div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .min_w_0()
            .truncate()
            .text_size(crate::typography::ui_rems(10.0))
            .text_color(theme.text_faint)
            .child(SharedString::from(text))
            .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into())
            .into_any_element()
    });
    let status = issue_row_icons(
        format!("outline-linear-{key}"),
        &issue.identifier,
        issue.entry.as_ref(),
        issue.url.clone(),
        shell,
        visible.dot,
        theme,
    );
    let group_selector = format!("outline-linear-slots-{key}");
    div()
        .id(SharedString::from(group_selector.clone()))
        .debug_selector(move || group_selector.clone())
        .flex_none()
        .flex()
        .items_center()
        .gap(px(SLOT_GAP))
        .when(visible.assignee, |el| {
            el.child(crate::github::row_icons::slot(
                format!("outline-linear-slot-assignee-{key}"),
                ASSIGNEE_WIDTH,
                true,
                assignee,
            ))
        })
        .child(crate::github::row_icons::slot(
            format!("outline-linear-slot-needs-{key}"),
            NEEDS_WIDTH,
            false,
            attention.map(|indicator| {
                let selector = format!("outline-linear-needs-{key}");
                div()
                    .debug_selector(move || selector.clone())
                    .flex_none()
                    .child(indicator)
                    .into_any_element()
            }),
        ))
        .child(crate::github::row_icons::slot(
            format!("outline-linear-slot-status-{key}"),
            status_width(visible.dot),
            false,
            Some(status),
        ))
        .tooltip(move |_, cx| cx.new(|_| TextTooltip(line.clone())).into())
        .into_any_element()
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
            (
                crate::linear::hex_color(&issue.state.color, theme.text_muted),
                issue_chip_text(issue),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linear::fixture::issue;

    #[test]
    fn status_icon_follows_the_state_type() {
        assert_eq!(
            linear_status_icon(LinearStateType::Completed),
            crate::icons::CHECK
        );
        assert_eq!(
            linear_status_icon(LinearStateType::Canceled),
            crate::icons::CLOSE_CIRCLE
        );
        assert_eq!(
            linear_status_icon(LinearStateType::Started),
            crate::icons::CLOCK_CIRCLE
        );
        for open in [
            LinearStateType::Triage,
            LinearStateType::Backlog,
            LinearStateType::Unstarted,
            LinearStateType::Unknown,
        ] {
            assert_eq!(linear_status_icon(open), crate::icons::CHECKLIST);
        }
    }

    #[test]
    fn tooltip_names_identifier_state_priority_and_assignee() {
        let mut ticket = issue("ENG-1", "Fix login");
        assert_eq!(
            issue_row_tooltip(&ticket),
            "ENG-1 \u{00b7} In Progress \u{00b7} High \u{00b7} Assigned to Mikey\nFix login"
        );
        ticket.assignee = None;
        ticket.priority = 0;
        assert_eq!(
            issue_row_tooltip(&ticket),
            "ENG-1 \u{00b7} In Progress \u{00b7} Unassigned\nFix login"
        );
    }

    #[test]
    fn narrow_rows_drop_the_dot_then_the_assignee_and_never_needs_or_status() {
        let all = LinearSlots::ALL;
        assert_eq!(linear_visible_slots(all.width()), all);
        assert_eq!(linear_visible_slots(1000.0), all);
        let no_dot = LinearSlots { dot: false, ..all };
        assert_eq!(linear_visible_slots(all.width() - 0.5), no_dot);
        assert_eq!(linear_visible_slots(no_dot.width()), no_dot);
        let minimal = LinearSlots {
            assignee: false,
            ..no_dot
        };
        assert_eq!(linear_visible_slots(no_dot.width() - 0.5), minimal);
        assert_eq!(linear_visible_slots(-50.0), minimal);
        assert!(minimal.width() > 0.0 && minimal.width() < no_dot.width());
        assert!(no_dot.width() < all.width());
    }

    #[test]
    fn group_tooltip_folds_in_the_chat_count_and_totals() {
        let ticket = issue("ENG-1", "Fix login");
        let entry = IssueEntry::Ready {
            issue: ticket.clone(),
            at: std::time::Instant::now(),
        };
        let base = issue_row_tooltip(&ticket);
        assert_eq!(linear_group_line("ENG-1", Some(&entry), 0, None), base);
        assert_eq!(
            linear_group_line("ENG-1", Some(&entry), 1, None),
            format!("{base}\n1 associated chat")
        );
        assert_eq!(
            linear_group_line(
                "ENG-1",
                Some(&entry),
                3,
                Some("Including descendants: 2 active")
            ),
            format!("{base}\n3 associated chats\nIncluding descendants: 2 active")
        );
        assert_eq!(
            linear_group_line("ENG-1", None, 0, None),
            "ENG-1 (loading from Linear)"
        );
    }
}
