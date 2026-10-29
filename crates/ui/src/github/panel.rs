//! The GitHub section of the Overview's left column (beside the Linear
//! panel; the Overview owns the column: `overview/linear_host.rs`,
//! `overview/github_host.rs`). Shown for Spaces where the PR review sync
//! applies (`github::space_available`).
//!
//! - The header's scope dropdown ([`menus`]): the project (when more than
//!   one applies) and a repo filter (All repos + each repo), ↻.
//! - The sync bar ([`sync_bar`]), the primary flow: pick (or create) the
//!   work item that PR review requests sync into; once set, "Syncing into
//!   <item> · last synced <age> · Sync now · Change ▾ · Stop".
//! - **Review requested** (`PrReviewRequested`) and **My open PRs**
//!   (`PrMyOpen`) for the panel's Space, each row with the PR chip info and
//!   its sync state: "Synced ✓" (a live item links it; the row's click
//!   reveals it), "Pending first sync" (a destination is set, the next pass
//!   adds it), or "Linked". Refreshed on open and by ↻ only. Both lists and
//!   the whole section fold (persisted per device: `prefs`).
//! - Secondary, manual actions in each row's ⋯ menu: **Start review**
//!   (`StartPrReview`, filed under the synced item when there is one, else
//!   under the sync destination), **Link to selected item**
//!   (`createWorkItemExternalReference`), **Create work item** (titled
//!   `<repo>#<n> · <title>` linking the PR, under the destination, else the
//!   selected item), and drag onto an Outline item (creates it there). Each
//!   action keeps one operation id per PR/target, so a retry replays instead
//!   of duplicating.
mod menus;
mod sync_bar;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use gpui::{
    AnyElement, Context, Entity, EventEmitter, FocusHandle, Focusable, SharedString, Window, div,
    prelude::*, px,
};
use mast_proto::{
    CreateWorkItemExternalReferenceParams, CreateWorkItemParams, GITHUB_PROVIDER,
    GITHUB_PULL_REQUEST_REFERENCE, PrReviewAction, PullRequestRef, PullRequestStatusSummary,
    WorkOutlinePlacement, WorkOutlineSnapshot,
};

pub(crate) use menus::PanelMenu;

use super::PrIndex;
use super::client::GithubUiError;
use super::prefs::{self, GithubList};
use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::linear::panel::{LinearPanel, OutlineSelection, PanelNotice};
use crate::popover::Popup;
use crate::state::AppState;
use crate::theme::Theme;
use crate::work_outline::actions::OperationSlot;
use crate::work_outline::chips::TextTooltip;
use crate::work_outline::outline_view::OutlineDrag;
use crate::work_outline::outline_view::pr_sync_outline::PrDrag;

/// One Space the section can show.
#[derive(Debug, Clone)]
pub(crate) struct GithubSpace {
    pub id: String,
    pub label: String,
    pub snapshot: Arc<WorkOutlineSnapshot>,
}

/// A list's load state.
#[derive(Debug, Clone)]
pub(crate) enum Load<T> {
    Loading,
    Ready(T),
    Failed(GithubUiError),
}

impl<T> Load<T> {
    pub(crate) fn ready(&self) -> Option<&T> {
        match self {
            Self::Ready(value) => Some(value),
            _ => None,
        }
    }

    fn error(&self) -> Option<&GithubUiError> {
        match self {
            Self::Failed(error) => Some(error),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GithubPanelEvent {
    Close,
    /// Select and reveal this item in the Outline.
    Reveal {
        space_id: String,
        item_id: String,
    },
    /// A create landed: select and reveal the new item once it shows.
    Created {
        space_id: String,
        item_id: String,
        parent_id: Option<String>,
    },
    /// Start a review of `pr`, filed under `item` when given.
    Review {
        pr: PullRequestRef,
        item: Option<(String, String)>,
        action: PrReviewAction,
    },
}

pub(crate) struct GithubPanel {
    state: Entity<AppState>,
    focus: FocusHandle,
    pub(crate) space_id: String,
    spaces: Vec<GithubSpace>,
    selection: OutlineSelection,
    pub(crate) review_requested: Load<Vec<PullRequestStatusSummary>>,
    pub(crate) my_open: Load<Vec<PullRequestStatusSummary>>,
    /// The whole section folded to its header (persisted).
    pub(crate) folded: bool,
    /// Folded lists (persisted).
    pub(crate) collapsed: BTreeSet<GithubList>,
    op_slots: HashMap<String, OperationSlot>,
    pub(crate) busy: HashSet<String>,
    pub(crate) notice: Option<PanelNotice>,
    generation: u64,
    /// Show only this repo's PRs (`owner/repo`, lowercased); `None` = all.
    pub(crate) repo_filter: Option<String>,
    menu: Popup<PanelMenu>,
    /// The open menu's keyboard cursor.
    pub(crate) menu_active: Option<usize>,
    destination_search: Entity<ComposerInput>,
    /// A destination change (or the "PR reviews" create) is in flight.
    pub(crate) destination_busy: bool,
    destination_op: OperationSlot,
    create_destination_op: OperationSlot,
    _subscriptions: Vec<gpui::Subscription>,
}

impl EventEmitter<GithubPanelEvent> for GithubPanel {}

impl Focusable for GithubPanel {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl GithubPanel {
    pub(crate) fn new(state: Entity<AppState>, space_id: String, cx: &mut Context<Self>) -> Self {
        let restored = state
            .read(cx)
            .data_dir
            .as_deref()
            .map(prefs::load_state)
            .unwrap_or_default();
        let destination_search = cx.new(|cx| {
            ComposerInput::with_context(
                "Search work items",
                crate::composer::PALETTE_SEARCH_CONTEXT,
                cx,
            )
            .with_single_line()
            .with_text_metrics(12.0, 18.0)
        });
        let subscriptions =
            vec![
                cx.subscribe(&destination_search, |this: &mut Self, _, event, cx| {
                    if matches!(event, ComposerInputEvent::Edited) {
                        // The cursor lands on the first matching item (row 0 is
                        // "Create 'PR reviews' item").
                        let count = this.menu_rows(cx).len();
                        this.menu_active = Some(usize::from(count > 1));
                        cx.notify();
                    }
                }),
            ];
        Self {
            state,
            focus: cx.focus_handle(),
            space_id,
            spaces: Vec::new(),
            selection: OutlineSelection::None,
            review_requested: Load::Loading,
            my_open: Load::Loading,
            folded: restored.folded,
            collapsed: restored.collapsed,
            op_slots: HashMap::new(),
            busy: HashSet::new(),
            notice: None,
            generation: 0,
            repo_filter: None,
            menu: Popup::default(),
            menu_active: None,
            destination_search,
            destination_busy: false,
            destination_op: OperationSlot::default(),
            create_destination_op: OperationSlot::default(),
            _subscriptions: subscriptions,
        }
    }

    /// Fed by the Overview every frame: the Spaces where the sync applies
    /// and the Outline's selection. A Space that dropped out moves the
    /// section to the first remaining one (and refetches).
    pub(crate) fn set_data(
        &mut self,
        spaces: Vec<GithubSpace>,
        selection: OutlineSelection,
        cx: &mut Context<Self>,
    ) {
        let moved = !spaces.iter().any(|space| space.id == self.space_id);
        if moved && let Some(first) = spaces.first() {
            self.space_id = first.id.clone();
            self.repo_filter = None;
        }
        self.spaces = spaces;
        self.selection = selection;
        if moved && !self.spaces.is_empty() {
            self.refresh(cx);
        }
    }

    pub(crate) fn set_space(&mut self, space_id: String, cx: &mut Context<Self>) {
        if self.space_id != space_id {
            self.space_id = space_id;
            self.repo_filter = None;
            self.refresh(cx);
        }
    }

    fn snapshot(&self) -> Option<&Arc<WorkOutlineSnapshot>> {
        self.spaces
            .iter()
            .find(|space| space.id == self.space_id)
            .map(|space| &space.snapshot)
    }

    /// Whether focus is anywhere inside the section.
    pub(crate) fn contains_focus(&self, window: &Window, cx: &gpui::App) -> bool {
        self.focus.contains_focused(window, cx)
    }

    pub(crate) fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
    }

    // ----------------------------------------------------------------------
    // Loading
    // ----------------------------------------------------------------------

    /// Re-fetch both lists for the panel's Space.
    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        let actions = self.state.read(cx).work_outline_actions();
        self.review_requested = Load::Loading;
        self.my_open = Load::Loading;
        let requested = actions.pr_review_requested(&self.space_id);
        let mine = actions.pr_my_open(&self.space_id);
        cx.spawn(async move |this, cx| {
            let requested = requested.await;
            let mine = mine.await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                for list in [&requested, &mine].into_iter().flatten() {
                    this.state
                        .update(cx, |state, _| state.github.remember(list.iter()));
                }
                this.review_requested = match requested {
                    Ok(list) => Load::Ready(list),
                    Err(error) => Load::Failed(error),
                };
                this.my_open = match mine {
                    Ok(list) => Load::Ready(list),
                    Err(error) => Load::Failed(error),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn save_state(&self, cx: &gpui::App) {
        let Some(data_dir) = self.state.read(cx).data_dir.clone() else {
            return;
        };
        prefs::save_state(
            &data_dir,
            &prefs::GithubPanelState {
                folded: self.folded,
                collapsed: self.collapsed.clone(),
            },
        );
    }

    pub(crate) fn toggle_folded(&mut self, cx: &mut Context<Self>) {
        self.folded = !self.folded;
        self.save_state(cx);
        cx.notify();
    }

    pub(crate) fn toggle_list(&mut self, list: GithubList, cx: &mut Context<Self>) {
        if !self.collapsed.remove(&list) {
            self.collapsed.insert(list);
        }
        self.save_state(cx);
        cx.notify();
    }

    // ----------------------------------------------------------------------
    // Actions
    // ----------------------------------------------------------------------

    fn set_notice(&mut self, text: impl Into<String>, error: bool) {
        self.notice = Some(PanelNotice {
            text: text.into(),
            error,
        });
    }

    fn selected_item(&self) -> Option<String> {
        match &self.selection {
            OutlineSelection::Item { space_id, item_id } if *space_id == self.space_id => self
                .snapshot()
                .and_then(|snapshot| snapshot.item(item_id))
                .filter(|item| item.archived_at.is_none())
                .map(|item| item.id.clone()),
            _ => None,
        }
    }

    /// The item a PR's review is filed under: the item that links it, else
    /// the sync destination.
    pub(crate) fn review_item(&self, pr: &PullRequestRef) -> Option<(String, String)> {
        let snapshot = self.snapshot()?;
        let item = PrIndex::from_snapshot(snapshot)
            .item(pr)
            .map(str::to_owned)
            .or_else(|| super::live_destination(snapshot).map(str::to_owned))?;
        Some((self.space_id.clone(), item))
    }

    /// Where Create puts a PR's work item: the destination, else the
    /// selected item.
    pub(crate) fn create_parent(&self) -> Option<(String, String)> {
        let snapshot = self.snapshot()?;
        let parent = super::live_destination(snapshot)
            .map(str::to_owned)
            .or_else(|| self.selected_item())?;
        let title = snapshot.item(&parent)?.title.clone();
        Some((parent, title))
    }

    pub(crate) fn review(
        &mut self,
        pr: PullRequestRef,
        action: PrReviewAction,
        cx: &mut Context<Self>,
    ) {
        let item = self.review_item(&pr);
        cx.emit(GithubPanelEvent::Review { pr, item, action });
    }

    /// Create a work item for `pr` under the destination (else the
    /// selected item).
    pub(crate) fn create(&mut self, pr: PullRequestStatusSummary, cx: &mut Context<Self>) {
        let Some((parent, _)) = self.create_parent() else {
            self.set_notice(
                "Pick a PR review destination or select a work item first",
                false,
            );
            cx.notify();
            return;
        };
        let space_id = self.space_id.clone();
        self.create_under(space_id, Some(parent), pr, cx);
    }

    /// `createWorkItem` (titled `<repo>#<n> · <title>`) then its
    /// `github` / `pullRequest` reference, under `parent` (placement Last).
    /// Also the Outline drop's entry point.
    pub(crate) fn create_under(
        &mut self,
        space_id: String,
        parent: Option<String>,
        pr: PullRequestStatusSummary,
        cx: &mut Context<Self>,
    ) {
        let external_id = pr.pr_ref.external_id();
        let busy = format!("create:{}", pr.pr_ref.key());
        if !self.busy.insert(busy.clone()) {
            return;
        }
        let inputs = format!("{space_id}\u{1f}{parent:?}\u{1f}{external_id}");
        let operation_id = self
            .op_slots
            .entry(busy.clone())
            .or_default()
            .id_for(&inputs);
        let actions = self.state.read(cx).work_outline_actions();
        let create = actions.create_pr_work_item(CreateWorkItemParams {
            space_id: space_id.clone(),
            parent_work_item_id: parent.clone(),
            placement: WorkOutlinePlacement::Last,
            title: mast_proto::pr_item_title(&pr.pr_ref.repo, pr.pr_ref.number, &pr.title),
            description: None,
            status: None,
            operation_id: operation_id.clone(),
        });
        self.notice = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = match create.await {
                Ok(reply) => {
                    let item_id = reply.work_item_id.unwrap_or_else(|| {
                        mast_proto::work_outline::derive_id(&operation_id, "item")
                    });
                    let link = actions.link_pull_request(CreateWorkItemExternalReferenceParams {
                        work_item_id: item_id.clone(),
                        provider: GITHUB_PROVIDER.into(),
                        reference_type: GITHUB_PULL_REQUEST_REFERENCE.into(),
                        external_id: external_id.clone(),
                        url: pr.pr_ref.url.clone(),
                        cached_title: Some(pr.title.clone()),
                        operation_id: mast_proto::work_outline::derive_id(&operation_id, "pr-link"),
                    });
                    link.await.map(|_| item_id)
                }
                Err(error) => Err(error),
            };
            this.update(cx, |this, cx| {
                this.busy.remove(&busy);
                match result {
                    Ok(item_id) => {
                        this.set_notice(format!("Created a work item for {external_id}"), false);
                        cx.emit(GithubPanelEvent::Created {
                            space_id: space_id.clone(),
                            item_id,
                            parent_id: parent.clone(),
                        });
                    }
                    Err(error) => this.set_notice(
                        format!(
                            "Couldn\u{2019}t create {external_id}: {}",
                            error.user_text()
                        ),
                        true,
                    ),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Link `pr` to the Outline's selected item.
    pub(crate) fn link(&mut self, pr: PullRequestStatusSummary, cx: &mut Context<Self>) {
        let Some(item_id) = self.selected_item() else {
            self.set_notice("Select a work item in the Outline first", false);
            cx.notify();
            return;
        };
        let external_id = pr.pr_ref.external_id();
        let busy = format!("link:{}", pr.pr_ref.key());
        if !self.busy.insert(busy.clone()) {
            return;
        }
        let operation_id = self
            .op_slots
            .entry(busy.clone())
            .or_default()
            .id_for(&item_id);
        let task = self
            .state
            .read(cx)
            .work_outline_actions()
            .link_pull_request(CreateWorkItemExternalReferenceParams {
                work_item_id: item_id,
                provider: GITHUB_PROVIDER.into(),
                reference_type: GITHUB_PULL_REQUEST_REFERENCE.into(),
                external_id: external_id.clone(),
                url: pr.pr_ref.url.clone(),
                cached_title: Some(pr.title.clone()),
                operation_id,
            });
        self.notice = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.busy.remove(&busy);
                match result {
                    Ok(_) => this.set_notice(format!("Linked {external_id}"), false),
                    Err(error) => this.set_notice(
                        format!("Couldn\u{2019}t link {external_id}: {}", error.user_text()),
                        true,
                    ),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ----------------------------------------------------------------------
    // Rendering
    // ----------------------------------------------------------------------

    fn quiet(text: impl Into<SharedString>, theme: &Theme) -> gpui::Div {
        div()
            .py(px(4.0))
            .text_size(crate::typography::ui_rems(11.5))
            .text_color(theme.text_faint)
            .child(text.into())
    }

    /// The account-wide blocker both lists share, if any.
    fn blocker(&self) -> Option<GithubUiError> {
        let error = self.review_requested.error().or(self.my_open.error())?;
        error.is_blocking().then(|| error.clone())
    }

    fn list(&self, list: GithubList) -> &Load<Vec<PullRequestStatusSummary>> {
        match list {
            GithubList::ReviewRequested => &self.review_requested,
            GithubList::MyOpen => &self.my_open,
        }
    }

    fn render_list_header(
        &self,
        list: GithubList,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = !self.collapsed.contains(&list);
        let label = match self.visible(list) {
            Some(all) => format!("{} ({})", list.label(), all.len()),
            None => list.label().to_string(),
        };
        let selector = format!("github-section-{}", list.key());
        div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .mt(px(6.0))
            .flex()
            .items_center()
            .gap(px(4.0))
            .px(px(2.0))
            .py(px(2.0))
            .rounded(px(4.0))
            .cursor_pointer()
            .text_size(crate::typography::ui_rems(11.0))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(theme.text_muted)
            .hover(|el| el.text_color(theme.text))
            .child(
                crate::icons::icon(if open {
                    crate::icons::ALT_ARROW_DOWN
                } else {
                    crate::icons::ALT_ARROW_RIGHT
                })
                .size(px(10.0))
                .text_color(theme.text_muted),
            )
            .child(SharedString::from(label))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_list(list, cx)))
            .into_any_element()
    }

    /// `list`'s PRs under the repo filter (once loaded).
    pub(crate) fn visible(&self, list: GithubList) -> Option<Vec<PullRequestStatusSummary>> {
        let all = self.list(list).ready()?;
        Some(
            all.iter()
                .filter(|pr| {
                    self.repo_filter
                        .as_ref()
                        .is_none_or(|key| menus::repo_key(&pr.pr_ref) == *key)
                })
                .cloned()
                .collect(),
        )
    }

    /// A PR row's sync state.
    pub(crate) fn row_sync(
        &self,
        list: GithubList,
        pr: &PullRequestStatusSummary,
        index: &PrIndex,
    ) -> RowSync {
        let snapshot = self.snapshot();
        let destination = snapshot.and_then(|snapshot| super::live_destination(snapshot));
        if let Some(item_id) = index.item(&pr.pr_ref) {
            let synced = snapshot
                .and_then(|snapshot| snapshot.item(item_id))
                .is_some_and(|item| {
                    destination
                        .is_some_and(|dest| item.parent_work_item_id.as_deref() == Some(dest))
                        || item
                            .external_sync_state
                            .as_ref()
                            .is_some_and(|state| state.provider == GITHUB_PROVIDER)
                });
            return if synced {
                RowSync::Synced(item_id.to_string())
            } else {
                RowSync::Linked(item_id.to_string())
            };
        }
        if destination.is_some() && list == GithubList::ReviewRequested {
            RowSync::Pending
        } else {
            RowSync::Unlinked
        }
    }

    /// The row's sync marker: Synced ✓ / Linked (click reveals), Pending
    /// first sync.
    fn sync_marker(
        &self,
        key: &str,
        sync: &RowSync,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (id, label, color, tip): (String, &'static str, gpui::Hsla, SharedString) = match sync {
            RowSync::Synced(_) => (
                format!("github-pr-synced-{key}"),
                "Synced \u{2713}",
                theme.accent,
                "Synced into a work item: click the row to reveal it".into(),
            ),
            RowSync::Linked(_) => (
                format!("github-pr-linked-{key}"),
                "Linked",
                theme.text_muted,
                "A work item in this project links it: click the row to reveal it".into(),
            ),
            RowSync::Pending => (
                format!("github-pr-pending-{key}"),
                "Pending first sync",
                theme.text_faint,
                "The next sync pass (every 3 minutes, or Sync now) adds a work item for it".into(),
            ),
            RowSync::Unlinked => return None,
        };
        let reveal = match sync {
            RowSync::Synced(item) | RowSync::Linked(item) => Some(item.clone()),
            _ => None,
        };
        let space_id = self.space_id.clone();
        let selector = id.clone();
        Some(
            div()
                .id(SharedString::from(id))
                .debug_selector(move || selector.clone())
                .flex_none()
                .px(px(4.0))
                .rounded(px(3.0))
                .text_size(crate::typography::ui_rems(10.0))
                .text_color(color)
                .child(label)
                .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into())
                .when_some(reveal, |el, item_id| {
                    el.cursor_pointer()
                        .hover(|el| el.bg(theme.element_hover))
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.stop_propagation();
                            cx.emit(GithubPanelEvent::Reveal {
                                space_id: space_id.clone(),
                                item_id: item_id.clone(),
                            });
                        }))
                })
                .into_any_element(),
        )
    }

    /// The row's ⋯ trigger and its menu.
    fn row_menu_trigger(
        &self,
        list: GithubList,
        pr: &PullRequestStatusSummary,
        key: &str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let menu = PanelMenu::Row {
            list,
            pr: Box::new(pr.clone()),
        };
        let selector = format!("github-pr-more-{key}");
        let id = selector.clone();
        let press = menu.clone();
        let open = menu.clone();
        let busy = self.busy.contains(&format!("create:{}", pr.pr_ref.key()))
            || self.busy.contains(&format!("link:{}", pr.pr_ref.key()));
        div()
            .relative()
            .flex_none()
            .child(
                div()
                    .id(SharedString::from(id))
                    .debug_selector(move || selector.clone())
                    .size(px(20.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.0))
                    .cursor_pointer()
                    .hover(|el| el.bg(theme.element_hover))
                    .child(
                        crate::icons::icon(crate::icons::MORE_HORIZONTAL)
                            .size(px(12.0))
                            .text_color(if busy { theme.accent } else { theme.text_faint }),
                    )
                    .tooltip(|_, cx| {
                        cx.new(|_| TextTooltip("Review, link or create a work item by hand".into()))
                            .into()
                    })
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |this, _, _, _| this.note_menu_press(&press)),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.toggle_menu(open.clone(), window, cx)
                    })),
            )
            .children(self.render_menu_for(&menu, theme, cx))
            .into_any_element()
    }

    fn render_row(
        &self,
        list: GithubList,
        pr: &PullRequestStatusSummary,
        index: &PrIndex,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = row_key(list, pr);
        let row_selector = format!("github-pr-row-{key}");
        let sync = self.row_sync(list, pr, index);
        let marker = self.sync_marker(&key, &sync, theme, cx);
        let reveal = match &sync {
            RowSync::Synced(item) | RowSync::Linked(item) => Some(item.clone()),
            _ => None,
        };
        let drag = OutlineDrag::PullRequest(PrDrag {
            space_id: self.space_id.clone(),
            pr: Box::new(pr.clone()),
        });
        let meta = {
            let mut parts = vec![format!("{}#{}", pr.pr_ref.repo, pr.pr_ref.number)];
            if !pr.author.is_empty() {
                parts.push(format!("by {}", pr.author));
            }
            parts.push(super::chip::chip_text(pr));
            parts.join(" \u{00b7} ")
        };
        let state_color = super::chip::state_color(pr.state, theme);
        let tip: SharedString = super::chip::tooltip_text(pr).into();
        let space_id = self.space_id.clone();
        div()
            .id(SharedString::from(row_selector.clone()))
            .debug_selector(move || row_selector.clone())
            .flex()
            .items_center()
            .gap(px(6.0))
            .px(px(4.0))
            .py(px(3.0))
            .rounded(px(4.0))
            .hover(|el| el.bg(theme.element_hover.opacity(0.6)))
            .when(reveal.is_none(), |el| el.cursor_grab())
            .when_some(reveal, |el, item_id| {
                el.cursor_pointer()
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(GithubPanelEvent::Reveal {
                            space_id: space_id.clone(),
                            item_id: item_id.clone(),
                        })
                    }))
            })
            .on_drag(drag, |drag, _, _, cx| {
                cx.stop_propagation();
                let title = match drag {
                    OutlineDrag::PullRequest(drag) => mast_proto::pr_item_title(
                        &drag.pr.pr_ref.repo,
                        drag.pr.pr_ref.number,
                        &drag.pr.title,
                    ),
                    _ => String::new(),
                };
                cx.new(|_| PrDragGhost {
                    title: title.into(),
                })
            })
            .child(
                crate::icons::icon(crate::icons::PULL_REQUEST)
                    .size(px(11.0))
                    .text_color(state_color),
            )
            .child(
                div()
                    .id(SharedString::from(format!("github-pr-text-{key}")))
                    .flex_1()
                    .min_w(px(90.0))
                    .flex()
                    .flex_col()
                    .gap(px(1.0))
                    .child(
                        div()
                            .truncate()
                            .text_size(crate::typography::ui_rems(12.0))
                            .text_color(theme.text)
                            .child(SharedString::from(pr.title.clone())),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(crate::typography::ui_rems(10.5))
                            .text_color(theme.text_faint)
                            .child(SharedString::from(meta)),
                    )
                    .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into()),
            )
            .children(marker)
            .child(self.row_menu_trigger(list, pr, &key, theme, cx))
            .into_any_element()
    }
}

/// A PR row's sync state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RowSync {
    /// The item the sync keeps for it (under the destination, or written
    /// by the sync).
    Synced(String),
    /// Another live item links it.
    Linked(String),
    /// A destination is set and nothing links it yet: the next pass adds it.
    Pending,
    Unlinked,
}

/// A row's selector key: `<repo>-<n>-<list>`, lowercased.
pub(crate) fn row_key(list: GithubList, pr: &PullRequestStatusSummary) -> String {
    format!("{}-{}-{}", pr.pr_ref.repo, pr.pr_ref.number, list.key())
        .to_lowercase()
        .replace(['/', '#'], "-")
}

/// The ghost that follows the pointer while dragging a PR row.
struct PrDragGhost {
    title: SharedString,
}

impl Render for PrDragGhost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        div()
            .max_w(px(260.0))
            .px(px(8.0))
            .py(px(3.0))
            .rounded(px(4.0))
            .border_1()
            .border_color(theme.border_strong)
            .bg(theme.surface_raised)
            .text_size(crate::typography::ui_rems(12.0))
            .text_color(theme.text)
            .truncate()
            .child(self.title.clone())
    }
}

impl Render for GithubPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let snapshot = self.snapshot().cloned();
        let index = snapshot
            .as_deref()
            .map(PrIndex::from_snapshot)
            .unwrap_or_default();
        let folded = self.folded;
        let scope = div()
            .relative()
            .flex_1()
            .min_w_0()
            .flex()
            .child(
                div()
                    .id("github-panel-space")
                    .debug_selector(|| "github-panel-space".into())
                    .min_w_0()
                    .truncate()
                    .px(px(4.0))
                    .rounded(px(4.0))
                    .cursor_pointer()
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.text_muted)
                    .hover(|el| el.text_color(theme.text).bg(theme.element_hover))
                    .child(SharedString::from(format!(
                        "{} \u{25be}",
                        self.scope_label()
                    )))
                    .tooltip(|_, cx| {
                        cx.new(|_| TextTooltip("Show one repo\u{2019}s PRs, or all".into()))
                            .into()
                    })
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(|this, _, _, _| this.note_menu_press(&PanelMenu::Scope)),
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.toggle_menu(PanelMenu::Scope, window, cx)
                    })),
            )
            .children(self.render_menu_for(&PanelMenu::Scope, &theme, cx));
        let header = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(6.0))
            .px(px(14.0))
            .pt(px(10.0))
            .child(
                div()
                    .id("github-panel-fold")
                    .debug_selector(|| "github-panel-fold".into())
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .cursor_pointer()
                    .text_size(crate::typography::ui_rems(13.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child(
                        crate::icons::icon(if folded {
                            crate::icons::ALT_ARROW_RIGHT
                        } else {
                            crate::icons::ALT_ARROW_DOWN
                        })
                        .size(px(10.0))
                        .text_color(theme.text_muted),
                    )
                    .child("GitHub")
                    .tooltip(move |_, cx| {
                        cx.new(|_| {
                            TextTooltip(if folded {
                                "Show the GitHub section".into()
                            } else {
                                "Fold the GitHub section".into()
                            })
                        })
                        .into()
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_folded(cx))),
            )
            .child(scope)
            .child(
                div()
                    .id("github-panel-refresh")
                    .debug_selector(|| "github-panel-refresh".into())
                    .flex_none()
                    .size(px(22.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.0))
                    .cursor_pointer()
                    .hover(|el| el.bg(theme.element_hover))
                    .child(
                        crate::icons::icon(crate::icons::REFRESH)
                            .size(px(12.0))
                            .text_color(theme.text_muted),
                    )
                    .tooltip(|_, cx| cx.new(|_| TextTooltip("Refresh from GitHub".into())).into())
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            );
        let blocker = self.blocker();
        // The sync needs the host's PR review sync: no bar where it is
        // missing.
        let sync_bar = (!folded && !blocker.as_ref().is_some_and(GithubUiError::not_available))
            .then_some(snapshot.as_deref())
            .flatten()
            .map(|snapshot| self.render_sync_bar(snapshot, &theme, cx));
        let mut body = div()
            .id("github-panel-body")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .px(px(14.0))
            .py(px(6.0));
        if !folded {
            if let Some(notice) = self.notice.clone() {
                body = body.child(
                    div()
                        .debug_selector(|| "github-panel-notice".into())
                        .text_size(crate::typography::ui_rems(11.5))
                        .text_color(if notice.error {
                            theme.warning
                        } else {
                            theme.text_muted
                        })
                        .child(SharedString::from(notice.text)),
                );
            }
            if let Some(error) = blocker {
                body = body.child(
                    div()
                        .debug_selector(|| "github-panel-blocker".into())
                        .py(px(6.0))
                        .text_size(crate::typography::ui_rems(12.0))
                        .text_color(theme.text_muted)
                        .child(SharedString::from(error.user_text())),
                );
            } else {
                for list in GithubList::ALL {
                    body = body.child(self.render_list_header(list, &theme, cx));
                    if self.collapsed.contains(&list) {
                        continue;
                    }
                    match self.list(list).clone() {
                        Load::Loading => body = body.child(Self::quiet("Loading\u{2026}", &theme)),
                        Load::Failed(error) => {
                            body = body.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .text_size(crate::typography::ui_rems(11.5))
                                            .text_color(theme.warning)
                                            .child(SharedString::from(error.user_text())),
                                    )
                                    .child(
                                        LinearPanel::small_button(
                                            SharedString::from(format!(
                                                "github-{}-retry",
                                                list.key()
                                            )),
                                            "Retry",
                                            &theme,
                                        )
                                        .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                                    ),
                            )
                        }
                        Load::Ready(_) => {
                            let shown = self.visible(list).unwrap_or_default();
                            if shown.is_empty() {
                                body = body.child(Self::quiet(
                                    match (list, self.repo_filter.is_some()) {
                                        (GithubList::ReviewRequested, false) => {
                                            "No PRs are waiting on your review"
                                        }
                                        (GithubList::MyOpen, false) => "No open PRs of yours",
                                        (GithubList::ReviewRequested, true) => {
                                            "No PRs in this repo are waiting on your review"
                                        }
                                        (GithubList::MyOpen, true) => {
                                            "No open PRs of yours in this repo"
                                        }
                                    },
                                    &theme,
                                ));
                            }
                            for pr in &shown {
                                body = body.child(self.render_row(list, pr, &index, &theme, cx));
                            }
                        }
                    }
                }
            }
        }
        div()
            .id("github-panel")
            .debug_selector(|| "github-panel".into())
            .track_focus(&self.focus)
            .when(!folded, |el| el.flex_1().min_h_0())
            .when(folded, |el| el.flex_none().pb(px(8.0)))
            .w_full()
            .flex()
            .flex_col()
            .capture_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                this.menu_key_down(event, window, cx)
            }))
            .on_key_down(cx.listener(|_, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.emit(GithubPanelEvent::Close);
                    cx.stop_propagation();
                }
            }))
            .child(header)
            .children(sync_bar)
            .when(!folded, |el| el.child(body))
    }
}
