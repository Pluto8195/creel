//! The GitHub section's dropdowns (one open at a time, `GithubPanel::menu`),
//! all built on `popover::choice_menu`:
//!
//! - **Scope** (the header chip): the project (when more than one applies)
//!   and the repository filter: All repos + every repo of the project with
//!   PRs in either list or linked from its Outline. Picking filters both
//!   lists; nothing cycles.
//! - **Destination** (the sync bar's Choose item ▾ / Change ▾): search,
//!   Create "PR reviews" item, then every live work item of the project.
//! - **Row** (a PR row's ⋯): Reveal item, Start review, Link to selected
//!   item, Create work item: the manual actions, secondary to the sync.
//!
//! ↑/↓ move the cursor, ↩ picks, Esc closes (back to the section).
use std::collections::BTreeMap;
use std::rc::Rc;

use gpui::{AnyElement, Context, Focusable as _, SharedString, Window, div, prelude::*, px};
use zeron_proto::{PrReviewAction, PullRequestRef, PullRequestStatusSummary};

use super::{GithubPanel, GithubPanelEvent, row_key};
use crate::github::PrIndex;
use crate::github::prefs::GithubList;
use crate::popover::choice_menu::{self, ChoiceKey, ChoiceRow};
use crate::theme::Theme;
use crate::work_outline::model;

/// Which dropdown is open.
#[derive(Debug, Clone)]
pub(crate) enum PanelMenu {
    Scope,
    Destination,
    Row {
        list: GithubList,
        pr: Box<PullRequestStatusSummary>,
    },
}

impl PanelMenu {
    fn same_trigger(&self, other: &PanelMenu) -> bool {
        match (self, other) {
            (Self::Scope, Self::Scope) | (Self::Destination, Self::Destination) => true,
            (Self::Row { list: a, pr: pa }, Self::Row { list: b, pr: pb }) => {
                a == b && pa.pr_ref.key() == pb.pr_ref.key()
            }
            _ => false,
        }
    }
}

/// What a menu row does.
#[derive(Debug, Clone)]
pub(crate) enum MenuChoice {
    Space(String),
    /// Filter by repo key (`owner/repo`, lowercased); `None` = All repos.
    Repo(Option<String>),
    CreateDestination,
    Destination(String),
    Reveal(String),
    Review(PullRequestRef),
    Link(Box<PullRequestStatusSummary>),
    Create(Box<PullRequestStatusSummary>),
}

/// The repo key a PR filters under.
pub(crate) fn repo_key(pr: &PullRequestRef) -> String {
    format!("{}/{}", pr.owner, pr.repo).to_lowercase()
}

fn selector_part(text: &str) -> String {
    text.to_lowercase().replace(['/', '#', ' '], "-")
}

/// The title the "Create 'PR reviews' item" choice gives the new item.
pub(crate) const DESTINATION_TITLE: &str = "PR reviews";

impl GithubPanel {
    /// The open menu (not one playing its exit animation).
    #[cfg(test)]
    pub(crate) fn open_menu_kind(&self) -> Option<&PanelMenu> {
        self.menu.as_open()
    }

    /// The repos the Scope menu offers: every repo with a PR in either list
    /// or a PR linked from the project's Outline, keyed `owner/repo`
    /// (lowercased), labelled by the repo name (`owner/repo` when two
    /// owners share a name), sorted by label.
    pub(crate) fn repos(&self) -> Vec<(String, String)> {
        let mut seen: BTreeMap<String, PullRequestRef> = BTreeMap::new();
        for list in [&self.review_requested, &self.my_open] {
            for pr in list.ready().into_iter().flatten() {
                seen.entry(repo_key(&pr.pr_ref))
                    .or_insert_with(|| pr.pr_ref.clone());
            }
        }
        if let Some(snapshot) = self.snapshot() {
            for reference in &snapshot.external_references {
                if reference.provider != zeron_proto::GITHUB_PROVIDER
                    || reference.reference_type != zeron_proto::GITHUB_PULL_REQUEST_REFERENCE
                {
                    continue;
                }
                if let Some(pr) = PullRequestRef::parse_external_id(&reference.external_id) {
                    seen.entry(repo_key(&pr)).or_insert(pr);
                }
            }
        }
        let mut names: BTreeMap<String, usize> = BTreeMap::new();
        for pr in seen.values() {
            *names.entry(pr.repo.to_lowercase()).or_default() += 1;
        }
        let mut repos: Vec<(String, String)> = seen
            .into_iter()
            .map(|(key, pr)| {
                let label = if names.get(&pr.repo.to_lowercase()).copied() > Some(1) {
                    format!("{}/{}", pr.owner, pr.repo)
                } else {
                    pr.repo.clone()
                };
                (key, label)
            })
            .collect();
        repos.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));
        repos
    }

    /// The Scope chip's label: the repo filter (and the project when more
    /// than one applies).
    pub(crate) fn scope_label(&self) -> String {
        let repo = self
            .repo_filter
            .as_ref()
            .and_then(|key| {
                self.repos()
                    .into_iter()
                    .find(|(k, _)| k == key)
                    .map(|(_, label)| label)
            })
            .unwrap_or_else(|| "All repos".to_string());
        if self.spaces.len() > 1 {
            let space = self
                .spaces
                .iter()
                .find(|space| space.id == self.space_id)
                .map(|space| space.label.clone())
                .unwrap_or_default();
            format!("{space} \u{00b7} {repo}")
        } else {
            repo
        }
    }

    fn destination_query(&self, cx: &gpui::App) -> String {
        self.destination_search.read(cx).text().trim().to_string()
    }

    /// The rows of the open menu, with what each does.
    pub(crate) fn menu_rows(&self, cx: &gpui::App) -> Vec<(ChoiceRow, MenuChoice)> {
        let Some(menu) = self.menu.get() else {
            return Vec::new();
        };
        match menu {
            PanelMenu::Scope => self.scope_rows(),
            PanelMenu::Destination => self.destination_rows(cx),
            PanelMenu::Row { list, pr } => self.row_menu_rows(*list, pr),
        }
    }

    fn scope_rows(&self) -> Vec<(ChoiceRow, MenuChoice)> {
        let mut rows = Vec::new();
        let multi = self.spaces.len() > 1;
        if multi {
            for (ix, space) in self.spaces.iter().enumerate() {
                let mut row = ChoiceRow::new(
                    format!("github-scope-space-{}", selector_part(&space.id)),
                    space.label.clone(),
                )
                .checked(space.id == self.space_id);
                if ix == 0 {
                    row = row.heading("Project");
                }
                rows.push((row, MenuChoice::Space(space.id.clone())));
            }
        }
        let mut all = ChoiceRow::new("github-scope-repo-all", "All repos")
            .checked(self.repo_filter.is_none())
            .heading("Repository");
        if multi {
            all = all.separated();
        }
        rows.push((all, MenuChoice::Repo(None)));
        for (key, label) in self.repos() {
            rows.push((
                ChoiceRow::new(format!("github-scope-repo-{}", selector_part(&key)), label)
                    .checked(self.repo_filter.as_deref() == Some(key.as_str())),
                MenuChoice::Repo(Some(key)),
            ));
        }
        rows
    }

    fn destination_rows(&self, cx: &gpui::App) -> Vec<(ChoiceRow, MenuChoice)> {
        let query = self.destination_query(cx);
        let mut rows = vec![(
            ChoiceRow::new(
                "github-sync-create",
                format!("Create \u{201c}{DESTINATION_TITLE}\u{201d} item"),
            )
            .icon(crate::icons::PLUS),
            MenuChoice::CreateDestination,
        )];
        let Some(snapshot) = self.snapshot() else {
            return rows;
        };
        let current = snapshot.pr_review_sync_destination();
        let items: Vec<_> = snapshot
            .items
            .iter()
            .filter(|item| item.archived_at.is_none())
            .map(|item| {
                let label = model::path_label(
                    model::item_path(snapshot, &item.id)
                        .iter()
                        .map(|item| item.title.as_str()),
                );
                (item.id.clone(), label)
            })
            .collect();
        let labels: Vec<&str> = items.iter().map(|(_, label)| label.as_str()).collect();
        for (n, ix) in crate::popover::filter_indices(&query, &labels)
            .into_iter()
            .enumerate()
        {
            let (id, label) = &items[ix];
            let mut row = ChoiceRow::new(format!("github-sync-pick-{id}"), label.clone())
                .checked(current == Some(id.as_str()));
            if n == 0 {
                row = row.separated();
            }
            rows.push((row, MenuChoice::Destination(id.clone())));
        }
        rows
    }

    fn row_menu_rows(
        &self,
        list: GithubList,
        pr: &PullRequestStatusSummary,
    ) -> Vec<(ChoiceRow, MenuChoice)> {
        let key = row_key(list, pr);
        let mut rows = Vec::new();
        let linked = self.snapshot().and_then(|snapshot| {
            PrIndex::from_snapshot(snapshot)
                .item(&pr.pr_ref)
                .map(str::to_owned)
        });
        if let Some(item_id) = linked {
            rows.push((
                ChoiceRow::new(format!("github-pr-reveal-{key}"), "Reveal item")
                    .icon(crate::icons::EYE),
                MenuChoice::Reveal(item_id),
            ));
        }
        let linking = self.busy.contains(&format!("link:{}", pr.pr_ref.key()));
        let creating = self.busy.contains(&format!("create:{}", pr.pr_ref.key()));
        rows.push((
            ChoiceRow::new(format!("github-pr-review-{key}"), "Start review")
                .icon(crate::icons::PULL_REQUEST),
            MenuChoice::Review(pr.pr_ref.clone()),
        ));
        rows.push((
            ChoiceRow::new(
                format!("github-pr-link-{key}"),
                if linking {
                    "Linking\u{2026}"
                } else {
                    "Link to selected item"
                },
            )
            .icon(crate::icons::PAPERCLIP)
            .separated(),
            MenuChoice::Link(Box::new(pr.clone())),
        ));
        let mut create = ChoiceRow::new(
            format!("github-pr-create-{key}"),
            if creating {
                "Creating\u{2026}"
            } else {
                "Create work item"
            },
        )
        .icon(crate::icons::DOCUMENT_ADD);
        if let Some((_, title)) = self.create_parent() {
            create = create.detail(format!("in {title}"));
        }
        rows.push((create, MenuChoice::Create(Box::new(pr.clone()))));
        rows
    }

    /// Open `menu` (a click on its trigger while it is open closes it).
    pub(crate) fn toggle_menu(
        &mut self,
        menu: PanelMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.menu.take_press_was_open() {
            self.close_menu(window, cx);
            return;
        }
        self.open_menu(menu, window, cx);
    }

    pub(crate) fn open_menu(
        &mut self,
        menu: PanelMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let destination = matches!(menu, PanelMenu::Destination);
        self.menu.open(menu);
        if destination {
            self.destination_search
                .update(cx, |input, cx| input.set_text("", cx));
            let handle = self.destination_search.focus_handle(cx);
            window.focus(&handle, cx);
        } else {
            window.focus(&self.focus, cx);
        }
        let rows: Vec<ChoiceRow> = self.menu_rows(cx).into_iter().map(|(row, _)| row).collect();
        self.menu_active = choice_menu::initial_active(&rows);
        cx.notify();
    }

    /// Close the open menu; focus returns to the section.
    pub(crate) fn close_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if crate::motion::reduced_motion(cx) {
            // No exit animation: unmount now.
            self.menu = crate::popover::Popup::default();
        } else if self.menu.begin_close() {
            crate::popover::reap_popup(cx, |this: &mut Self| &mut this.menu);
        }
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Note a press on a trigger (so its click toggles rather than reopens).
    pub(crate) fn note_menu_press(&mut self, menu: &PanelMenu) {
        self.menu
            .note_trigger_press_matching(|open| open.same_trigger(menu));
    }

    /// Run the open menu's row `ix`.
    pub(crate) fn pick_menu_row(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some((_, choice)) = self.menu_rows(cx).into_iter().nth(ix) else {
            return;
        };
        self.close_menu(window, cx);
        match choice {
            MenuChoice::Space(space_id) => self.set_space(space_id, cx),
            MenuChoice::Repo(filter) => {
                self.repo_filter = filter;
                cx.notify();
            }
            MenuChoice::CreateDestination => self.create_destination(cx),
            MenuChoice::Destination(item_id) => self.set_destination(Some(item_id), cx),
            MenuChoice::Reveal(item_id) => cx.emit(GithubPanelEvent::Reveal {
                space_id: self.space_id.clone(),
                item_id,
            }),
            MenuChoice::Review(pr) => self.review(pr, PrReviewAction::CodeReview, cx),
            MenuChoice::Link(pr) => self.link(*pr, cx),
            MenuChoice::Create(pr) => self.create(*pr, cx),
        }
    }

    /// ↑/↓/↩/Esc while a menu is open (capture phase, so the search box
    /// keeps every other key).
    pub(crate) fn menu_key_down(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.menu.as_open().is_none() {
            return;
        }
        let count = self.menu_rows(cx).len();
        match choice_menu::choice_key(&event.keystroke, self.menu_active, count) {
            ChoiceKey::Moved(active) => {
                cx.stop_propagation();
                self.menu_active = active;
                cx.notify();
            }
            ChoiceKey::Pick(ix) => {
                cx.stop_propagation();
                self.pick_menu_row(ix, window, cx);
            }
            ChoiceKey::Close => {
                cx.stop_propagation();
                self.close_menu(window, cx);
            }
            ChoiceKey::Ignored => {}
        }
    }

    /// The anchored card of the open menu when `trigger` owns it.
    pub(crate) fn render_menu_for(
        &self,
        trigger: &PanelMenu,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let open = self.menu.get()?;
        if !open.same_trigger(trigger) {
            return None;
        }
        let closing = self.menu.closing_since();
        let popup = theme.for_popup();
        let rows: Vec<ChoiceRow> = self.menu_rows(cx).into_iter().map(|(row, _)| row).collect();
        let (id, width) = match open {
            PanelMenu::Scope => ("github-scope-menu", 220.0),
            PanelMenu::Destination => ("github-sync-menu", 280.0),
            PanelMenu::Row { .. } => ("github-pr-menu", 220.0),
        };
        let pick: Rc<dyn Fn(&mut Self, usize, &mut Window, &mut Context<Self>)> =
            Rc::new(|this, ix, window, cx| this.pick_menu_row(ix, window, cx));
        let body = choice_menu::choice_menu_body(&rows, self.menu_active, theme, pick, cx);
        let card = crate::popover::popover_card(&popup)
            .debug_selector(move || id.to_string())
            .w(px(width))
            .flex()
            .flex_col()
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close_menu(window, cx)))
            .when(matches!(open, PanelMenu::Destination), |el| {
                el.child(crate::popover::search_input_frame(
                    &popup,
                    div()
                        .debug_selector(|| "github-sync-search".into())
                        .child(self.destination_search.clone())
                        .into_any_element(),
                ))
            })
            .child(
                div()
                    .id(SharedString::from(format!("{id}-list")))
                    .max_h(px(280.0))
                    .overflow_y_scroll()
                    .child(body),
            );
        Some(crate::popover::anchored_menu_below(
            id,
            card.into_any_element(),
            closing,
        ))
    }
}
