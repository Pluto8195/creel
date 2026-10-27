//! "Review PR" entry points on the shell: `StartPrReview` with the
//! composer's agent, then open the new Chat (docs/pr-review.md).

use super::*;
use zeron_proto::{
    PrReference, PrReviewAction, PrReviewError, PrReviewErrorCode, StartPrReviewReply,
};

/// Tests: answers `StartPrReview` params in place of the engine.
#[cfg(test)]
pub(crate) type PrReviewScript =
    std::sync::Arc<dyn Fn(&serde_json::Value) -> Result<serde_json::Value, String> + Send + Sync>;

impl Shell {
    /// Every PR entry point (outline ⌘↩, the context menu, Details, the PR
    /// page's Review / Fix CI, the palette's Review PR) opens the Start Chat
    /// dialog in PR mode: the review prompt, the PR link and the checkout
    /// setup the host chooses, with the real model picker. Nothing starts
    /// until the dialog's Start; see [`Self::launch_pr_review`]. `item`
    /// (`(spaceId, workItemId)`) files the new Chat under that work item.
    /// Opening an existing review Chat never comes through here.
    pub(crate) fn open_start_pr_review(
        &mut self,
        reference: PrReference,
        action: PrReviewAction,
        item: Option<(String, String)>,
        cx: &mut Context<Self>,
    ) -> Result<(), &'static str> {
        self.open_start_chat_for_pr(reference, action, item, cx)
    }

    /// `StartPrReview` for `reference` with the agent picked in the dialog:
    /// the host checks the PR out like the Cofactr launcher and creates a
    /// `pr_review` Chat. On success the Chat opens and any behind-origin
    /// warning shows in the sidebar notice; the returned task carries the
    /// outcome so the dialog can show refusals.
    pub(super) fn launch_pr_review(
        &mut self,
        reference: PrReference,
        action: PrReviewAction,
        item: Option<(String, String)>,
        config: Option<zeron_proto::ChatConfig>,
        cx: &mut Context<Self>,
    ) -> Task<Result<StartPrReviewReply, PrReviewError>> {
        let mut params = self
            .pr_review_ops
            .params_in_item(&reference, action, config, item);
        // The user confirmed this start with an agent pick: not the live
        // review Chat of the same PR, which would ignore it.
        params.fresh = true;

        #[cfg(test)]
        if let Some(script) = self.pr_review_script.clone() {
            let result = serde_json::to_value(&params)
                .map_err(|e| e.to_string())
                .and_then(|value| script(&value))
                .map_err(|text| {
                    PrReviewError::from_wire_message(&text).unwrap_or_else(|| {
                        PrReviewError::new(PrReviewErrorCode::DeviceOffline, text)
                    })
                })
                .and_then(|value| {
                    serde_json::from_value::<StartPrReviewReply>(value)
                        .map_err(|e| PrReviewError::new(PrReviewErrorCode::Invalid, e.to_string()))
                });
            if let Ok(reply) = &result {
                self.pr_review_started(reply, cx);
            }
            return Task::ready(result);
        }

        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return Task::ready(Err(PrReviewError::new(
                PrReviewErrorCode::DeviceOffline,
                "Engine not connected",
            )));
        };
        cx.spawn(async move |this, cx| {
            let result = crate::pr_review_launch::start(engine, params).await;
            if let Ok(reply) = &result {
                this.update(cx, |shell, cx| shell.pr_review_started(reply, cx))
                    .ok();
            }
            result
        })
    }

    fn pr_review_started(&mut self, reply: &StartPrReviewReply, cx: &mut Context<Self>) {
        if let Some(notice) = crate::pr_review_launch::started_notice(reply) {
            self.sidebar_notice = Some(notice.into());
        }
        self.open_chat(reply.chat_id.clone(), cx);
    }

    /// Palette "Review PR…": the ⇧⌘P page with its URL box focused.
    pub(super) fn open_review_pr_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_pr_review_chats(cx);
        let page = self.ensure_pr_review_chats_page(cx);
        page.update(cx, |page, cx| page.focus_review_input(window, cx));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat_type_view::ReviewStatus;
    use crate::pr_page::fixtures::row;
    use crate::pr_page::PageTab;
    use std::sync::{Arc, Mutex};
    use zeron_proto::{ChatLinkSource, PrDashBucket, PrDashChat, PrDashRow, PrDashTab};

    type Calls = Arc<Mutex<Vec<serde_json::Value>>>;

    fn test_shell<'a>(
        cx: &'a mut gpui::TestAppContext,
        dir: &std::path::Path,
    ) -> (Entity<Shell>, &'a mut gpui::VisualTestContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
        });
        let data_dir = dir.to_path_buf();
        cx.add_window_view(move |_, cx| {
            let state = cx.new(|_| AppState::new());
            state.update(cx, |state, _| {
                state.connection = crate::state::ConnectionStatus::Ready;
                state.workspace_scope = Some(WorkspaceScope::Local);
                state.auth = Some(zeron_proto::AuthState::SignedOut);
            });
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: data_dir.clone(),
                    ipc_port: 0,
                    edge_url: "http://127.0.0.1:1".into(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: zeron_proto::HarnessId::Mock,
                },
                cx,
            )
        })
    }

    /// A leaked selector for `debug_bounds` (tests only).
    fn sel(text: String) -> &'static str {
        Box::leak(text.into_boxed_str())
    }

    fn draw(cx: &mut gpui::VisualTestContext) {
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear());
    }

    fn click(cx: &mut gpui::VisualTestContext, selector: &'static str) {
        let at = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is drawn"))
            .center();
        cx.simulate_click(at, gpui::Modifiers::default());
        draw(cx);
    }

    fn reply(chat: &str, behind: u64) -> serde_json::Value {
        serde_json::json!({
            "chatId": chat, "spaceId": "s1", "deviceId": "d",
            "title": "Review flagship#7: Add X",
            "prUrl": "https://github.com/acme/flagship/pull/7",
            "branch": "feature-x", "checkoutPath": "/work/ws/flagship",
            "checkoutKind": "canonicalCheckout", "cwd": "/work/ws",
            "behindBy": behind, "prompt": "/code-review …",
        })
    }

    const T1: &str = "2026-10-20T10:00:00Z";
    const T2: &str = "2026-10-21T10:00:00Z";
    const T3: &str = "2026-10-22T10:00:00Z";

    fn chat_link(id: &str, title: &str, archived: bool, is_review: bool) -> PrDashChat {
        PrDashChat {
            chat_id: id.into(),
            title: title.into(),
            // A review chat is linked by the launcher; any other chat is
            // only mentioned the PR.
            source: if is_review {
                ChatLinkSource::ReviewLauncher
            } else {
                ChatLinkSource::Mentioned
            },
            archived,
            is_review,
            updated_at: None,
        }
    }

    /// Review-requested fixtures: #7 and #8 wait on me (so the two review
    /// buttons the older tests click are rows 0 and 1), #9 is approved and
    /// already has a review chat; #7 also links a plain work chat.
    fn review_rows() -> Vec<PrDashRow> {
        let mut seven = row(7, PrDashBucket::ToReview, T3);
        seven.title = "Add X".into();
        seven.chats = vec![chat_link("chat-work", "Implement X", false, false)];
        let mut eight = row(8, PrDashBucket::ToReview, T2);
        eight.title = "Fix Y".into();
        let mut nine = row(9, PrDashBucket::Approved, T1);
        nine.title = "Tidy Z".into();
        nine.chats = vec![
            chat_link("chat-old-review", "Old review of Z", true, true),
            chat_link("chat-review-9", "Review flagship#9", false, true),
        ];
        vec![seven, eight, nine]
    }

    /// Authored fixtures: two needs-you rows (#21 newer than #22), a
    /// needs-you draft, one in review and one ready.
    fn authored_rows() -> Vec<PrDashRow> {
        let mut needs = row(21, PrDashBucket::NeedsYou, T3);
        needs.reasons = vec!["2 checks failing".into()];
        needs.ci = Some("FAILURE".into());
        let older = row(22, PrDashBucket::NeedsYou, T1);
        let mut draft = row(24, PrDashBucket::NeedsYou, T3);
        draft.draft = true;
        vec![
            needs,
            older,
            row(23, PrDashBucket::InReview, T2),
            draft,
            row(25, PrDashBucket::Ready, T2),
        ]
    }

    fn dash_reply(rows: Vec<PrDashRow>) -> serde_json::Value {
        serde_json::json!({
            "me": "octo", "rows": rows, "fetchedAt": "2026-10-23T12:00:00Z",
        })
    }

    type DashRows = Arc<Mutex<(Vec<PrDashRow>, Vec<PrDashRow>)>>;

    /// Answer `PrDashboard` from `rows` (authored, review requested),
    /// recording every call.
    fn script_dash(shell: &Entity<Shell>, cx: &mut gpui::VisualTestContext) -> (Calls, DashRows) {
        let calls: Calls = Arc::default();
        let rows: DashRows = Arc::new(Mutex::new((authored_rows(), review_rows())));
        let (sink, source) = (calls.clone(), rows.clone());
        shell.update(cx, |shell, _| {
            shell.pr_dash_script = Some(Arc::new(move |params| {
                sink.lock().unwrap().push(params.clone());
                let rows = source.lock().unwrap();
                Ok(dash_reply(if params["tab"] == "authored" {
                    rows.0.clone()
                } else {
                    rows.1.clone()
                }))
            }));
        });
        (calls, rows)
    }

    /// Script `StartPrReview` and `PrDashboard`, then open ⇧⌘P on `tab`.
    fn open_page_on(
        shell: &Entity<Shell>,
        cx: &mut gpui::VisualTestContext,
        tab: PageTab,
        answer: impl Fn(&serde_json::Value) -> Result<serde_json::Value, String> + Send + Sync + 'static,
    ) -> (Calls, Entity<crate::chat_type_view::ChatTypeView>) {
        let calls: Calls = Arc::default();
        let sink = calls.clone();
        script_dash(shell, cx);
        let page = shell.update(cx, |shell, cx| {
            shell.pr_review_script = Some(Arc::new(move |params| {
                sink.lock().unwrap().push(params.clone());
                answer(params)
            }));
            shell.open_pr_review_chats(cx);
            shell.ensure_pr_review_chats_page(cx)
        });
        draw(cx);
        page.update(cx, |page, cx| page.set_tab(tab, cx));
        draw(cx);
        (calls, page)
    }

    /// ⇧⌘P on the "Review requested" tab (#7, #8, #9).
    fn open_page(
        shell: &Entity<Shell>,
        cx: &mut gpui::VisualTestContext,
        answer: impl Fn(&serde_json::Value) -> Result<serde_json::Value, String> + Send + Sync + 'static,
    ) -> (Calls, Entity<crate::chat_type_view::ChatTypeView>) {
        open_page_on(
            shell,
            cx,
            PageTab::Dash(PrDashTab::ReviewRequested),
            answer,
        )
    }

    #[gpui::test]
    fn review_requested_rows_render_and_review_starts_once_per_request(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let (calls, _page) = open_page(&shell, cx, |_| Ok(reply("chat-review", 2)));
        for selector in [
            "pr-dash-row-0",
            "pr-dash-review-0",
            "pr-dash-fix-ci-0",
            "pr-dash-row-1",
            "pr-review-url-input",
            "pr-review-url-submit",
        ] {
            assert!(cx.debug_bounds(selector).is_some(), "{selector} renders");
        }

        click(cx, "pr-dash-review-0");
        let first = calls.lock().unwrap()[0].clone();
        assert_eq!(first["repoOwner"], "acme");
        assert_eq!(first["repoName"], "flagship");
        assert_eq!(first["number"], 7);
        assert_eq!(first["action"], "codeReview");
        let op = first["operationId"].as_str().unwrap().to_owned();
        assert!(op.starts_with("pr-review-"));
        shell.read_with(cx, |shell, cx| {
            assert!(matches!(shell.route, Route::Chat));
            assert_eq!(
                shell.state.read(cx).selected_chat.as_deref(),
                Some("chat-review")
            );
            let notice = shell.sidebar_notice.clone().expect("behind warning");
            assert!(
                notice.contains("2 commit(s) behind origin/feature-x"),
                "{notice}"
            );
        });

        // The same request again (a double-click, a retry) reuses the id;
        // Fix CI is another request.
        shell.update(cx, |shell, cx| shell.open_pr_review_chats(cx));
        draw(cx);
        click(cx, "pr-dash-review-0");
        shell.update(cx, |shell, cx| shell.open_pr_review_chats(cx));
        draw(cx);
        click(cx, "pr-dash-fix-ci-0");
        let calls = calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[1]["operationId"], op.as_str());
        assert_eq!(calls[2]["action"], "fixCi");
        assert_ne!(calls[2]["operationId"], op.as_str());
    }

    #[gpui::test]
    fn refusals_stay_inline_on_the_row(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let (_calls, page) = open_page(&shell, cx, |_| {
            Err("pr-review/fork: PR 8 comes from a fork; check it out by hand".into())
        });
        click(cx, "pr-dash-review-1");
        shell.read_with(cx, |shell, _| {
            assert!(
                matches!(shell.route, Route::PrReviewChats),
                "stays on the page"
            );
        });
        let status = page.read_with(cx, |page, _| {
            page.review_status("https://github.com/acme/flagship/pull/8")
        });
        match status {
            Some(ReviewStatus::Failed(text)) => {
                assert!(text.contains("comes from a fork"), "{text}");
                assert!(text.contains("check the branch out by hand"), "{text}");
            }
            other => panic!("expected an inline failure, got {other:?}"),
        }
        assert!(cx.debug_bounds("pr-dash-status-1").is_some());
    }

    /// The git worktree failure Mikey hit, verbatim: multi-line stderr.
    const WORKTREE_FAILURE: &str = "pr-review/worktree: git worktree add /work/ws/.wt/pr-8 failed\nPreparing worktree (new branch 'pr-8')\ngit-lfs filter-process: git-lfs: command not found\nfatal: the remote end hung up unexpectedly";

    #[gpui::test]
    fn a_failed_launch_shows_a_copyable_error_notice_with_the_full_detail(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let (_calls, page) = open_page(&shell, cx, |_| Err(WORKTREE_FAILURE.into()));
        click(cx, "pr-dash-review-1");
        let text = match page.read_with(cx, |page, _| {
            page.review_status("https://github.com/acme/flagship/pull/8")
        }) {
            Some(ReviewStatus::Failed(text)) => text,
            other => panic!("expected a failure, got {other:?}"),
        };
        // Nothing is truncated in the model: every stderr line is there.
        for line in [
            "git worktree add /work/ws/.wt/pr-8 failed",
            "Preparing worktree (new branch 'pr-8')",
            "git-lfs: command not found",
            "fatal: the remote end hung up unexpectedly",
        ] {
            assert!(text.contains(line), "{line} in {text}");
        }
        // Rendered as the shared error card, with its Copy button.
        assert!(cx.debug_bounds("pr-dash-status-1").is_some());
        assert!(
            cx.debug_bounds("pr-dash-status-1-error")
                .is_some()
        );
        click(cx, "pr-dash-status-1-error-copy");
        let copied = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .unwrap();
        assert_eq!(
            copied,
            format!("{}\n\n{text}", crate::chat_type_view::REVIEW_FAILURE_TITLE)
        );
        assert!(copied.contains("hung up unexpectedly"));
        // It stays: a later frame does not clear it.
        draw(cx);
        assert!(
            cx.debug_bounds("pr-dash-status-1-error")
                .is_some()
        );
    }

    #[gpui::test]
    fn the_url_box_parses_urls_and_short_references(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let (calls, page) = open_page(&shell, cx, |_| Ok(reply("chat-url", 0)));
        let input = page.read_with(cx, |page, _| page.review_input());

        input.update(cx, |input, cx| input.set_text("not a pr", cx));
        click(cx, "pr-review-url-submit");
        assert!(calls.lock().unwrap().is_empty());
        assert_eq!(
            page.read_with(cx, |page, _| page.input_status()),
            Some(ReviewStatus::Failed(
                crate::pr_review_launch::REFERENCE_HINT.to_owned()
            ))
        );
        assert!(cx.debug_bounds("pr-review-url-status").is_some());

        for (text, owner, name, number) in [
            ("https://github.com/o/r/pull/41", "o", "r", 41),
            ("o/r#42", "o", "r", 42),
        ] {
            shell.update(cx, |shell, cx| shell.open_pr_review_chats(cx));
            draw(cx);
            input.update(cx, |input, cx| input.set_text(text, cx));
            click(cx, "pr-review-url-submit");
            let last = calls.lock().unwrap().last().cloned().unwrap();
            assert_eq!(
                (
                    last["repoOwner"].as_str(),
                    last["repoName"].as_str(),
                    last["number"].as_u64(),
                    last["action"].as_str(),
                ),
                (Some(owner), Some(name), Some(number), Some("codeReview")),
                "{text}"
            );
            assert_eq!(input.read_with(cx, |input, _| input.text().to_owned()), "");
        }
        shell.read_with(cx, |shell, cx| {
            assert_eq!(
                shell.state.read(cx).selected_chat.as_deref(),
                Some("chat-url")
            );
            assert!(shell.sidebar_notice.is_none(), "no warning when current");
        });
    }

    /// ⇧⌘P with a PR review sync destination in the current Space: the
    /// destination's PR items (chip, review Chats nested) replace the
    /// launcher review chat list, and Review sends StartPrReview filed under the
    /// item (`spaceId` + `workItemId`).
    #[gpui::test]
    fn destination_items_replace_the_launcher_review_chat_list(cx: &mut gpui::TestAppContext) {
        use crate::work_outline::store::fixture_ids::{API, CHAT_API, LAUNCH};
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            // The Shell saves on state changes (the PR chip's status lands).
            settings::init(settings::UiSettings::default(), dir.path(), cx);
            crate::history::init(
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                cx,
            );
        });
        let (shell, cx) = test_shell(cx, dir.path());
        let state = shell.read_with(cx, |shell, _| shell.state.clone());
        state.update(cx, |state, _| {
            state.spaces = vec![
                serde_json::from_value(serde_json::json!({
                    "id": "s1", "deviceId": "d", "path": "/work/alpha",
                    "createdAt": "2026-01-01T00:00:00Z"
                }))
                .unwrap(),
            ];
            state.selected_space = Some("s1".into());
            state.no_project = false;
            let mut chat: zeron_proto::Chat = serde_json::from_value(serde_json::json!({
                "id": CHAT_API, "deviceId": "d", "title": "Review launch#42", "archived": false,
                "createdAt": "2026-10-01T00:00:00Z"
            }))
            .unwrap();
            chat.space_id = Some("s1".into());
            state.chats = vec![chat];
            let mut snapshot = crate::work_outline::store::fixture_snapshot("s1");
            snapshot.space_settings.pr_review_sync_work_item_id = Some(LAUNCH.into());
            state.set_work_outline_for_test(snapshot);
        });
        let (calls, _page) = open_page_on(&shell, cx, PageTab::Chats, |_| {
            Ok(reply("chat-item-review", 0))
        });
        for selector in [
            "pr-destination-rows",
            "pr-destination-row-w-api",
            "pr-destination-chip-w-api",
            "pr-destination-chat-chat-api",
            "pr-destination-review-w-api",
        ] {
            assert!(cx.debug_bounds(selector).is_some(), "{selector} renders");
        }
        assert!(
            cx.debug_bounds("chat-type-rows").is_none(),
            "the launcher review chat list is replaced"
        );
        click(cx, "pr-destination-review-w-api");
        let calls = calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["repoOwner"], "acme");
        assert_eq!(calls[0]["repoName"], "launch");
        assert_eq!(calls[0]["number"], 42);
        assert_eq!(calls[0]["action"], "codeReview");
        assert_eq!(calls[0]["spaceId"], "s1");
        assert_eq!(calls[0]["workItemId"], API);
    }

    /// Without a destination the tab lists the chats the review launcher
    /// started: the ones with a PR link whose source is `review_launcher`.
    #[gpui::test]
    fn without_a_destination_the_list_shows_the_launcher_review_chats(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let state = shell.read_with(cx, |shell, _| shell.state.clone());
        state.update(cx, |state, _| {
            state.chats = [
                ("launched", "Review flagship#7", false),
                ("hand-linked", "Hand linked", false),
                ("archived-launch", "Old review", true),
                ("unlinked", "No link", false),
            ]
            .into_iter()
            .map(|(id, title, archived)| {
                serde_json::from_value(serde_json::json!({
                    "id": id, "deviceId": "d", "title": title, "archived": archived,
                    "createdAt": "2026-10-01T00:00:00Z"
                }))
                .unwrap()
            })
            .collect();
        });
        let (_calls, page) = open_page_on(&shell, cx, PageTab::Chats, |_| Ok(reply("chat", 0)));
        let link_status = |source: &str| -> zeron_engine::pr_ticket_cache::ChatLinkStatus {
            serde_json::from_value(serde_json::json!({
                "pr": null,
                "prLinks": [{
                    "url": "https://github.com/acme/flagship/pull/7",
                    "source": source,
                    "detail": null
                }],
                "ticket": null, "isWorktree": false, "diffStat": null,
                "prSource": null, "ticketSource": null
            }))
            .unwrap()
        };
        page.update(cx, |page, _| {
            page.set_links_for_test("launched", link_status("review_launcher"));
            page.set_links_for_test("hand-linked", link_status("manual"));
            page.set_links_for_test("archived-launch", link_status("review_launcher"));
        });
        draw(cx);
        assert!(cx.debug_bounds("pr-destination-rows").is_none());
        assert!(cx.debug_bounds("chat-type-rows").is_some());
        let listed = |cx: &mut gpui::VisualTestContext| {
            page.update(cx, |page, cx| page.listed_chat_ids(cx))
        };
        assert_eq!(listed(cx), ["launched"]);
        // The archived toggle adds the archived launcher chat.
        click(cx, "chat-type-show-archived");
        let mut with_archived = listed(cx);
        with_archived.sort();
        assert_eq!(with_archived, ["archived-launch", "launched"]);
    }

    // ---- the PR page's pr-dash tabs ----

    fn lines(
        page: &Entity<crate::chat_type_view::ChatTypeView>,
        cx: &mut gpui::VisualTestContext,
        tab: PrDashTab,
    ) -> Vec<String> {
        page.read_with(cx, |page, cx| page.dash_lines(tab, cx))
    }

    fn authored_page(
        shell: &Entity<Shell>,
        cx: &mut gpui::VisualTestContext,
    ) -> (Calls, Entity<crate::chat_type_view::ChatTypeView>) {
        open_page_on(shell, cx, PageTab::Dash(PrDashTab::Authored), |_| {
            Ok(reply("chat-review", 0))
        })
    }

    #[gpui::test]
    fn tabs_keep_their_own_rows_and_filters(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let (_calls, page) = authored_page(&shell, cx);
        // Default tab: Authored by me, grouped in bucket order, drafts hidden.
        assert_eq!(
            lines(&page, cx, PrDashTab::Authored),
            ["● Needs you · 2", "#21", "#22", "✓ Ready to merge · 1", "#25", "· In review · 1", "#23"]
        );
        for selector in ["pr-page-tab-authored", "pr-page-tab-review-requested", "pr-page-tab-chats"] {
            assert!(cx.debug_bounds(selector).is_some(), "{selector}");
        }
        assert!(cx.debug_bounds("pr-review-url-input").is_some(), "URL box on every tab");

        // A chip filter on this tab ...
        click(cx, "pr-dash-chip-authored-ready");
        assert_eq!(
            lines(&page, cx, PrDashTab::Authored),
            ["✓ Ready to merge · 1", "#25"]
        );
        // ... does not leak into the other tab, which shows its own rows.
        click(cx, "pr-page-tab-review-requested");
        assert_eq!(
            lines(&page, cx, PrDashTab::ReviewRequested),
            ["● Waiting on you · 2", "#7", "#8", "✓ Approved · 1", "#9"]
        );
        assert!(cx.debug_bounds("pr-dash-row-2").is_some());
        assert!(cx.debug_bounds("pr-dash-chip-review-requested-to-review").is_some());
        assert!(cx.debug_bounds("pr-dash-chip-authored-ready").is_none());
        // Back: the authored chip selection is still there.
        click(cx, "pr-page-tab-authored");
        assert_eq!(
            lines(&page, cx, PrDashTab::Authored),
            ["✓ Ready to merge · 1", "#25"]
        );
        // The chats tab keeps the old table.
        click(cx, "pr-page-tab-chats");
        assert!(cx.debug_bounds("chat-type-rows").is_some());
        assert!(cx.debug_bounds("pr-review-url-input").is_some());
        assert!(cx.debug_bounds("pr-dash-rows").is_none());
    }

    #[gpui::test]
    fn tab_badges_show_urgent_over_total_or_just_the_total(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let (_calls, page) = authored_page(&shell, cx);
        click(cx, "pr-page-tab-review-requested");
        // Authored: 2 needs-you of 4 admitted rows (the draft is hidden).
        assert_eq!(
            page.read_with(cx, |page, _| page.dash_badge(PrDashTab::Authored)),
            Some(crate::pr_page::Badge { text: "2 / 4".into(), alert: true })
        );
        assert!(cx.debug_bounds("pr-page-tab-badge-alert-authored").is_some());
        // Review requested: 2 waiting on me of 3.
        assert_eq!(
            page.read_with(cx, |page, _| page.dash_badge(PrDashTab::ReviewRequested)),
            Some(crate::pr_page::Badge { text: "2 / 3".into(), alert: true })
        );
        assert!(cx.debug_bounds("pr-page-tab-badge-alert-review-requested").is_some());

        // Nothing urgent anywhere: a plain count, no alert class.
        let calm = vec![row(31, PrDashBucket::InReview, T1), row(32, PrDashBucket::Approved, T2)];
        shell.update(cx, |shell, _| {
            shell.pr_dash_script = Some(Arc::new(move |_| Ok(dash_reply(calm.clone()))));
        });
        click(cx, "pr-dash-refresh");
        assert_eq!(
            page.read_with(cx, |page, _| page.dash_badge(PrDashTab::ReviewRequested)),
            Some(crate::pr_page::Badge { text: "2".into(), alert: false })
        );
        assert!(cx.debug_bounds("pr-page-tab-badge-review-requested").is_some());
        assert!(cx.debug_bounds("pr-page-tab-badge-alert-review-requested").is_none());
    }

    #[gpui::test]
    fn hide_drafts_group_toggle_and_filters(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let (_calls, page) = authored_page(&shell, cx);
        let authored = |page: &Entity<crate::chat_type_view::ChatTypeView>, cx: &mut gpui::VisualTestContext| {
            lines(page, cx, PrDashTab::Authored)
        };
        assert!(!authored(&page, cx).contains(&"#24".to_owned()), "drafts hidden by default");
        assert_eq!(page.read_with(cx, |page, cx| page.dash_summary(PrDashTab::Authored, cx)), "4 of 4");

        // Unchecking hide drafts admits the draft everywhere: list, chips,
        // summary and the badge.
        click(cx, "pr-dash-hide-drafts");
        assert_eq!(
            authored(&page, cx),
            ["● Needs you · 3", "#21", "#24", "#22", "✓ Ready to merge · 1", "#25", "· In review · 1", "#23"]
        );
        assert_eq!(page.read_with(cx, |page, cx| page.dash_summary(PrDashTab::Authored, cx)), "5 of 5");
        assert_eq!(
            page.read_with(cx, |page, _| page.dash_badge(PrDashTab::Authored)).unwrap().text,
            "3 / 5"
        );
        click(cx, "pr-dash-hide-drafts");
        assert_eq!(authored(&page, cx).len(), 7);

        // Group off: one flat list, newest first, no headers.
        click(cx, "pr-dash-group");
        assert_eq!(authored(&page, cx), ["#21", "#23", "#25", "#22"]);
        assert!(cx.debug_bounds("pr-dash-group-needs-you").is_none());
        click(cx, "pr-dash-group");
        assert!(cx.debug_bounds("pr-dash-group-needs-you").is_some());

        // Chips: one per bucket with rows; OR semantics; counts in the text.
        assert!(cx.debug_bounds("pr-dash-chip-authored-needs-you").is_some());
        assert!(cx.debug_bounds("pr-dash-chip-authored-comments").is_none());
        click(cx, "pr-dash-chip-authored-ready");
        click(cx, "pr-dash-chip-authored-in-review");
        assert_eq!(
            authored(&page, cx),
            ["✓ Ready to merge · 1", "#25", "· In review · 1", "#23"]
        );
        assert_eq!(page.read_with(cx, |page, cx| page.dash_summary(PrDashTab::Authored, cx)), "2 of 4");
        click(cx, "pr-dash-chip-authored-ready");
        assert_eq!(authored(&page, cx), ["· In review · 1", "#23"]);

        // Text search: every term must match (title, author, number, labels).
        click(cx, "pr-dash-chip-authored-in-review");
        let input = page.read_with(cx, |page, _| page.dash_search_input(PrDashTab::Authored));
        input.update(cx, |input, cx| input.set_text("pr 25", cx));
        draw(cx);
        assert_eq!(authored(&page, cx), ["✓ Ready to merge · 1", "#25"]);
        input.update(cx, |input, cx| input.set_text("octo nomatch", cx));
        draw(cx);
        assert!(authored(&page, cx).is_empty());
        assert!(cx.debug_bounds("pr-dash-empty").is_some());
    }

    #[gpui::test]
    fn the_page_shows_loading_then_errors_inline_and_keeps_the_last_rows(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let (_calls, page) = authored_page(&shell, cx);
        assert!(cx.debug_bounds("pr-dash-error").is_none());
        shell.update(cx, |shell, _| {
            shell.pr_dash_script = Some(Arc::new(|_| Err("gh: HTTP 502".into())));
        });
        click(cx, "pr-dash-refresh");
        assert!(cx.debug_bounds("pr-dash-error").is_some());
        assert_eq!(lines(&page, cx, PrDashTab::Authored).len(), 7, "last good rows stay");
        // A reply that carries `error` is shown the same way.
        shell.update(cx, |shell, _| {
            shell.pr_dash_script = Some(Arc::new(|_| {
                Ok(serde_json::json!({
                    "me": "octo", "rows": [], "fetchedAt": "2026-10-23T12:00:00Z",
                    "error": "rate limited"
                }))
            }));
        });
        click(cx, "pr-dash-refresh");
        assert!(cx.debug_bounds("pr-dash-error").is_some());
        assert!(cx.debug_bounds("pr-dash-empty").is_some());
        // A good reply clears it.
        shell.update(cx, |shell, _| {
            shell.pr_dash_script = Some(Arc::new(|_| Ok(dash_reply(vec![row(1, PrDashBucket::InReview, T1)]))));
        });
        click(cx, "pr-dash-refresh");
        assert!(cx.debug_bounds("pr-dash-error").is_none());
        assert_eq!(lines(&page, cx, PrDashTab::Authored), ["· In review · 1", "#1"]);
    }

    #[gpui::test]
    fn the_page_says_loading_until_the_first_reply(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        script_dash(&shell, cx);
        shell.update(cx, |shell, cx| shell.open_pr_review_chats(cx));
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
        assert!(cx.debug_bounds("pr-dash-loading").is_some());
        draw(cx);
        assert!(cx.debug_bounds("pr-dash-loading").is_none());
        assert!(cx.debug_bounds("pr-dash-row-0").is_some());
    }

    #[gpui::test]
    fn the_visible_tab_refetches_every_minute_only_while_on_screen(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let (_calls, _page) = authored_page(&shell, cx);
        let (calls, _rows) = script_dash(&shell, cx);
        let count = |calls: &Calls| {
            calls
                .lock()
                .unwrap()
                .iter()
                .filter(|call| call.get("tab").is_some())
                .count()
        };
        cx.executor().advance_clock(std::time::Duration::from_secs(61));
        cx.run_until_parked();
        assert_eq!(count(&calls), 1, "one refetch of the visible tab");
        assert_eq!(calls.lock().unwrap()[0]["tab"], "authored");
        assert_eq!(calls.lock().unwrap()[0]["force"], false);
        // Off the page the timer fetches nothing.
        shell.update(cx, |shell, cx| shell.open_overview(cx));
        draw(cx);
        cx.executor().advance_clock(std::time::Duration::from_secs(61));
        cx.run_until_parked();
        assert_eq!(count(&calls), 1);
    }

    #[gpui::test]
    fn the_refresh_button_forces(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let (_calls, _page) = authored_page(&shell, cx);
        let (calls, _rows) = script_dash(&shell, cx);
        click(cx, "pr-dash-refresh");
        let calls = calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["force"], true);
    }

    #[gpui::test]
    fn cmd_p_and_cmd_shift_p_open_the_pr_page(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        cx.update(|_, cx| {
            apply_keymap(
                cx,
                &crate::settings::KeymapConfig::default(),
                crate::settings::ComposerSendBehavior::default(),
            );
        });
        script_dash(&shell, cx);
        draw(cx);
        for combo in ["mod-p", "mod-shift-p"] {
            shell.update(cx, |shell, cx| shell.open_overview(cx));
            draw(cx);
            shell.read_with(cx, |shell, _| assert!(matches!(shell.route, Route::Overview)));
            cx.simulate_keystrokes(&platform_combo(combo));
            draw(cx);
            shell.read_with(cx, |shell, _| {
                assert!(matches!(shell.route, Route::PrReviewChats), "{combo}")
            });
            assert!(cx.debug_bounds("pr-page-tab-authored").is_some(), "{combo}");
        }
    }

    #[gpui::test]
    fn a_linked_chat_chip_opens_that_chat(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let (calls, _page) = open_page(&shell, cx, |_| Ok(reply("unused", 0)));
        // #7 links a work chat; #9 links an archived and a live review chat.
        for selector in ["pr-dash-chat-0-0", "pr-dash-chat-2-0", "pr-dash-chat-2-1"] {
            assert!(cx.debug_bounds(selector).is_some(), "{selector}");
        }
        assert!(cx.debug_bounds("pr-dash-chat-1-0").is_none(), "#8 has no chats");
        click(cx, "pr-dash-chat-0-0");
        shell.read_with(cx, |shell, cx| {
            assert!(matches!(shell.route, Route::Chat));
            assert_eq!(shell.state.read(cx).selected_chat.as_deref(), Some("chat-work"));
        });
        assert!(calls.lock().unwrap().is_empty(), "no review was started");
    }

    #[gpui::test]
    fn open_review_replaces_review_when_a_review_chat_exists(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let (calls, _page) = open_page(&shell, cx, |_| Ok(reply("chat-started", 0)));
        // #9 has a live review chat: Open review is the primary action and
        // Review is gone from the row.
        assert!(cx.debug_bounds("pr-dash-open-review-2").is_some());
        assert!(cx.debug_bounds("pr-dash-review-2").is_none());
        assert!(cx.debug_bounds("pr-dash-fix-ci-2").is_some());
        // The button names the chat and its live state.
        let detail = cx
            .debug_bounds("pr-dash-open-review-detail-2")
            .expect("the chat title and state render");
        assert!(detail.size.width > gpui::px(0.0));
        // #7 and #8 have none (the plain work chat only mentions #7).
        for ix in [0, 1] {
            assert!(cx.debug_bounds(sel(format!("pr-dash-review-{ix}"))).is_some());
            assert!(cx.debug_bounds(sel(format!("pr-dash-open-review-{ix}"))).is_none());
            assert!(cx.debug_bounds(sel(format!("pr-dash-more-{ix}"))).is_none());
        }
        click(cx, "pr-dash-open-review-2");
        assert!(calls.lock().unwrap().is_empty(), "StartPrReview is never called");
        shell.read_with(cx, |shell, cx| {
            assert!(matches!(shell.route, Route::Chat));
            assert_eq!(shell.state.read(cx).selected_chat.as_deref(), Some("chat-review-9"));
        });
        // A row with none starts one.
        shell.update(cx, |shell, cx| shell.open_pr_review_chats(cx));
        draw(cx);
        click(cx, "pr-dash-review-1");
        let calls = calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["number"], 8);
        shell.read_with(cx, |shell, cx| {
            assert_eq!(shell.state.read(cx).selected_chat.as_deref(), Some("chat-started"));
        });
    }

    #[gpui::test]
    fn the_overflow_menu_offers_start_another_review(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let (calls, _page) = open_page(&shell, cx, |_| Ok(reply("chat-second", 0)));
        assert!(cx.debug_bounds("pr-dash-start-another-2").is_none(), "closed by default");
        click(cx, "pr-dash-more-2");
        assert!(cx.debug_bounds("pr-dash-start-another-2").is_some());
        click(cx, "pr-dash-start-another-2");
        let calls = calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["number"], 9);
        assert_eq!(calls[0]["action"], "codeReview");
        shell.read_with(cx, |shell, cx| {
            assert_eq!(shell.state.read(cx).selected_chat.as_deref(), Some("chat-second"));
        });
    }

    #[gpui::test]
    fn the_menu_lists_the_other_review_chats_and_open_review_takes_the_latest(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let (calls, rows) = {
            let calls: Calls = Arc::default();
            let sink = calls.clone();
            let (_, rows) = script_dash(&shell, cx);
            shell.update(cx, |shell, _| {
                shell.pr_review_script = Some(Arc::new(move |params| {
                    sink.lock().unwrap().push(params.clone());
                    Ok(reply("unused", 0))
                }));
            });
            (calls, rows)
        };
        {
            let mut rows = rows.lock().unwrap();
            let nine = rows.1.iter_mut().find(|row| row.number == 9).unwrap();
            let mut older = chat_link("chat-review-old", "Earlier review", false, true);
            older.updated_at = Some(T1.into());
            let mut newer = chat_link("chat-review-new", "Second review", false, true);
            newer.updated_at = Some(T3.into());
            // A manually linked chat is a review chat too.
            let mut manual = chat_link("chat-manual", "Pinned by hand", false, false);
            manual.source = ChatLinkSource::Manual;
            manual.updated_at = Some(T2.into());
            nine.chats = vec![older, newer, manual];
        }
        shell.update(cx, |shell, cx| shell.open_pr_review_chats(cx));
        let page = shell.update(cx, |shell, cx| shell.ensure_pr_review_chats_page(cx));
        draw(cx);
        page.update(cx, |page, cx| {
            page.set_tab(PageTab::Dash(PrDashTab::ReviewRequested), cx)
        });
        draw(cx);
        click(cx, "pr-dash-more-2");
        // Most recent first: "Second review" opens, the others are listed.
        assert!(cx.debug_bounds("pr-dash-review-other-2-0").is_some());
        assert!(cx.debug_bounds("pr-dash-review-other-2-1").is_some());
        assert!(cx.debug_bounds("pr-dash-review-other-2-2").is_none());
        click(cx, "pr-dash-review-other-2-0");
        shell.read_with(cx, |shell, cx| {
            assert_eq!(shell.state.read(cx).selected_chat.as_deref(), Some("chat-manual"));
        });
        // Let the menu's exit animation finish before the next click (the
        // popup measures it in wall-clock time).
        std::thread::sleep(std::time::Duration::from_millis(400));
        cx.executor().advance_clock(std::time::Duration::from_secs(1));
        cx.run_until_parked();
        shell.update(cx, |shell, cx| shell.open_pr_review_chats(cx));
        draw(cx);
        assert!(cx.debug_bounds("pr-dash-start-another-2").is_none(), "the menu is gone");
        click(cx, "pr-dash-open-review-2");
        shell.read_with(cx, |shell, cx| {
            assert_eq!(
                shell.state.read(cx).selected_chat.as_deref(),
                Some("chat-review-new")
            );
        });
        assert!(calls.lock().unwrap().is_empty());
    }

    #[gpui::test]
    fn enter_opens_the_review_chat_or_starts_a_review(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let (calls, page) = open_page(&shell, cx, |_| Ok(reply("chat-started", 0)));
        let focus = page.read_with(cx, |page, _| page.focus_handle());
        cx.update(|window, cx| window.focus(&focus, cx));
        // Row 0 (#7) has no review chat: Enter starts a review.
        cx.simulate_keystrokes("enter");
        draw(cx);
        {
            let calls = calls.lock().unwrap();
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0]["number"], 7);
            assert_eq!(calls[0]["action"], "codeReview");
        }
        // Down twice: row 2 (#9) has one, so Enter opens it.
        shell.update(cx, |shell, cx| shell.open_pr_review_chats(cx));
        draw(cx);
        cx.update(|window, cx| window.focus(&focus, cx));
        cx.simulate_keystrokes("down down enter");
        draw(cx);
        assert_eq!(calls.lock().unwrap().len(), 1, "no second review was started");
        shell.read_with(cx, |shell, cx| {
            assert!(matches!(shell.route, Route::Chat));
            assert_eq!(shell.state.read(cx).selected_chat.as_deref(), Some("chat-review-9"));
        });
    }

    /// The authored tab with a review chat on #21 (row 0, CI failing); #22
    /// (row 1, CI passing) has none.
    fn authored_page_with_review_on_21(
        shell: &Entity<Shell>,
        cx: &mut gpui::VisualTestContext,
    ) -> (Calls, Entity<crate::chat_type_view::ChatTypeView>) {
        let calls: Calls = Arc::default();
        let sink = calls.clone();
        let (_, rows) = script_dash(shell, cx);
        rows.lock()
            .unwrap()
            .0
            .iter_mut()
            .find(|row| row.number == 21)
            .unwrap()
            .chats = vec![chat_link("chat-review-21", "Review flagship#21", false, true)];
        let page = shell.update(cx, |shell, cx| {
            shell.pr_review_script = Some(Arc::new(move |params| {
                sink.lock().unwrap().push(params.clone());
                Ok(reply("chat-started", 0))
            }));
            shell.open_pr_review_chats(cx);
            shell.ensure_pr_review_chats_page(cx)
        });
        draw(cx);
        page.update(cx, |page, cx| page.set_tab(PageTab::Dash(PrDashTab::Authored), cx));
        draw(cx);
        (calls, page)
    }

    #[gpui::test]
    fn authored_rows_offer_review_and_fix_ci_only_when_ci_is_failing(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let (calls, _page) = authored_page(&shell, cx);
        // No row has a review chat: each offers Review, none Open review or
        // the menu.
        for ix in 0..4 {
            assert!(cx.debug_bounds(sel(format!("pr-dash-review-{ix}"))).is_some());
            assert!(cx.debug_bounds(sel(format!("pr-dash-open-review-{ix}"))).is_none());
            assert!(cx.debug_bounds(sel(format!("pr-dash-more-{ix}"))).is_none());
        }
        // Row 0 is #21 (CI failing); #22 passes.
        assert!(cx.debug_bounds("pr-dash-fix-ci-0").is_some());
        assert!(cx.debug_bounds("pr-dash-fix-ci-1").is_none());
        click(cx, "pr-dash-fix-ci-0");
        {
            let calls = calls.lock().unwrap();
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0]["number"], 21);
            assert_eq!(calls[0]["action"], "fixCi");
        }
    }

    #[gpui::test]
    fn clicking_review_on_an_authored_row_starts_one_review(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let (calls, _page) = authored_page(&shell, cx);
        click(cx, "pr-dash-review-1");
        let calls = calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 1, "one StartPrReview");
        assert_eq!(calls[0]["repoOwner"], "acme");
        assert_eq!(calls[0]["repoName"], "flagship");
        assert_eq!(calls[0]["number"], 22);
        assert_eq!(calls[0]["action"], "codeReview");
        shell.read_with(cx, |shell, cx| {
            assert!(matches!(shell.route, Route::Chat));
            assert_eq!(shell.state.read(cx).selected_chat.as_deref(), Some("chat-review"));
        });
    }

    #[gpui::test]
    fn an_authored_row_with_a_review_chat_offers_open_review_the_menu_and_fix_ci(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let (calls, _page) = authored_page_with_review_on_21(&shell, cx);
        // #21: Open review replaces Review; Fix CI stays beside it (CI
        // fails) and the menu is there.
        assert!(cx.debug_bounds("pr-dash-open-review-0").is_some());
        assert!(cx.debug_bounds("pr-dash-open-review-detail-0").is_some());
        assert!(cx.debug_bounds("pr-dash-review-0").is_none());
        assert!(cx.debug_bounds("pr-dash-fix-ci-0").is_some());
        assert!(cx.debug_bounds("pr-dash-more-0").is_some());
        // #22 has no chat and passing CI: Review only.
        assert!(cx.debug_bounds("pr-dash-review-1").is_some());
        assert!(cx.debug_bounds("pr-dash-fix-ci-1").is_none());
        assert!(cx.debug_bounds("pr-dash-more-1").is_none());

        click(cx, "pr-dash-open-review-0");
        assert!(calls.lock().unwrap().is_empty(), "StartPrReview is never called");
        shell.read_with(cx, |shell, cx| {
            assert!(matches!(shell.route, Route::Chat));
            assert_eq!(shell.state.read(cx).selected_chat.as_deref(), Some("chat-review-21"));
        });

        // "Start another review" starts a code review of #21.
        shell.update(cx, |shell, cx| shell.open_pr_review_chats(cx));
        draw(cx);
        assert!(cx.debug_bounds("pr-dash-start-another-0").is_none(), "closed by default");
        click(cx, "pr-dash-more-0");
        click(cx, "pr-dash-start-another-0");
        let calls = calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["number"], 21);
        assert_eq!(calls[0]["action"], "codeReview");
    }

    #[gpui::test]
    fn enter_on_an_authored_row_opens_the_review_chat_or_starts_a_review(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let (calls, page) = authored_page_with_review_on_21(&shell, cx);
        let focus = page.read_with(cx, |page, _| page.focus_handle());
        cx.update(|window, cx| window.focus(&focus, cx));
        // Row 0 (#21) has a review chat: Enter opens it.
        cx.simulate_keystrokes("enter");
        draw(cx);
        assert!(calls.lock().unwrap().is_empty(), "no review was started");
        shell.read_with(cx, |shell, cx| {
            assert!(matches!(shell.route, Route::Chat));
            assert_eq!(shell.state.read(cx).selected_chat.as_deref(), Some("chat-review-21"));
        });
        // Row 1 (#22) has none: Enter starts a review.
        shell.update(cx, |shell, cx| shell.open_pr_review_chats(cx));
        draw(cx);
        cx.update(|window, cx| window.focus(&focus, cx));
        cx.simulate_keystrokes("down enter");
        draw(cx);
        let calls = calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["number"], 22);
        assert_eq!(calls[0]["action"], "codeReview");
    }

    fn add_review_chat(shell: &Entity<Shell>, cx: &mut gpui::VisualTestContext, id: &str) {
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, _| {
                state.chats.push(
                    serde_json::from_value(serde_json::json!({
                        "id": id, "deviceId": "d", "title": "Review flagship#21",
                        "archived": false, "createdAt": "2026-10-01T00:00:00Z"
                    }))
                    .unwrap(),
                )
            })
        });
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[gpui::test]
    fn the_number_opens_a_side_browser_beside_the_review_chat(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let (_calls, _page) = authored_page_with_review_on_21(&shell, cx);
        add_review_chat(&shell, cx, "chat-review-21");
        // The PR page has no side pane: #21 goes to its review chat, with
        // the browser beside it.
        click(cx, "pr-dash-number-0");
        cx.run_until_parked();
        assert!(cx.opened_url().is_none());
        shell.read_with(cx, |shell, cx| {
            assert!(matches!(shell.route, Route::Chat));
            assert_eq!(shell.state.read(cx).selected_chat.as_deref(), Some("chat-review-21"));
            assert_eq!(shell.active_chat, "chat-review-21");
            assert!(shell.right_pane_open(cx));
            let urls: Vec<_> = shell
                .browsers
                .values()
                .map(|browser| browser.read(cx).page.url.clone())
                .collect();
            assert_eq!(urls, [Some("https://github.com/acme/flagship/pull/21".to_string())]);
        });
        // Back on the page, the same PR focuses that tab instead of adding one.
        shell.update(cx, |shell, cx| shell.open_pr_review_chats(cx));
        draw(cx);
        click(cx, "pr-dash-number-0");
        cx.run_until_parked();
        shell.read_with(cx, |shell, _| assert_eq!(shell.browsers.len(), 1));
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[gpui::test]
    fn command_click_on_the_number_and_the_off_setting_stay_external(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let (_calls, _page) = authored_page_with_review_on_21(&shell, cx);
        add_review_chat(&shell, cx, "chat-review-21");
        let at = cx.debug_bounds("pr-dash-number-0").unwrap().center();
        cx.simulate_click(at, gpui::Modifiers::secondary_key());
        draw(cx);
        assert_eq!(
            cx.opened_url().as_deref(),
            Some("https://github.com/acme/flagship/pull/21")
        );
        shell.read_with(cx, |shell, _| {
            assert!(matches!(shell.route, Route::PrReviewChats), "the page stays");
            assert!(shell.browsers.is_empty());
        });
        // Setting off: a plain click is external too.
        cx.update(|_, cx| {
            crate::settings::update(crate::settings::SavePolicy::Immediate, cx, |settings| {
                settings.open_web_links_in_zeron = false
            })
        });
        click(cx, "pr-dash-number-1");
        assert_eq!(
            cx.opened_url().as_deref(),
            Some("https://github.com/acme/flagship/pull/22")
        );
        shell.read_with(cx, |shell, _| {
            assert!(matches!(shell.route, Route::PrReviewChats));
            assert!(shell.browsers.is_empty());
        });
    }

    #[gpui::test]
    fn the_number_opens_the_pr_in_the_browser(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let (_calls, _page) = authored_page(&shell, cx);
        click(cx, "pr-dash-number-0");
        assert_eq!(
            cx.opened_url().as_deref(),
            Some("https://github.com/acme/flagship/pull/21")
        );
    }
}
