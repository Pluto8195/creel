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
    /// Start reviewing `reference` (`action`): the host checks the PR out like
    /// the Cofactr launcher and creates a `pr_review` Chat at the workspace
    /// root. On success the Chat opens and any behind-origin warning shows in
    /// the sidebar notice; the returned task carries the outcome so the
    /// calling surface can show refusals inline.
    pub(crate) fn start_pr_review(
        &mut self,
        reference: PrReference,
        action: PrReviewAction,
        cx: &mut Context<Self>,
    ) -> Task<Result<StartPrReviewReply, PrReviewError>> {
        self.start_pr_review_in_item(reference, action, None, cx)
    }

    /// [`Self::start_pr_review`] filed under work item `item` (`(spaceId,
    /// workItemId)`): the new Chat is associated with that item (PR review
    /// sync: ⌘↩ on a PR item, the GitHub panel's Review).
    pub(crate) fn start_pr_review_in_item(
        &mut self,
        reference: PrReference,
        action: PrReviewAction,
        item: Option<(String, String)>,
        cx: &mut Context<Self>,
    ) -> Task<Result<StartPrReviewReply, PrReviewError>> {
        // The composer's agent for new chats: exactly what a brand-new
        // regular chat would start with (sticky harness, model, reasoning,
        // options) — never the selected chat's config. The host defaults to
        // Claude Code when nothing resolves.
        let pickers = self.composer.read(cx).pickers().clone();
        let config = crate::pickers::Pickers::new_chat_config(&pickers, cx);
        let params = self
            .pr_review_ops
            .params_in_item(&reference, action, config, item);

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
    use std::sync::{Arc, Mutex};
    use zeron_engine::pr_ticket_cache::{MyOpenPr, MyPrItem};

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

    fn item(number: u64, title: &str) -> MyPrItem {
        MyPrItem {
            summary: MyOpenPr {
                number,
                title: Some(title.into()),
                url: Some(format!("https://github.com/acme/flagship/pull/{number}")),
                repo: Some("acme/flagship".into()),
                is_draft: false,
                updated_at: None,
                author: Some("octo".into()),
            },
            detail: None,
        }
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

    /// Script `StartPrReview` and open ⇧⌘P with two review-requested PRs.
    fn open_page(
        shell: &Entity<Shell>,
        cx: &mut gpui::VisualTestContext,
        answer: impl Fn(&serde_json::Value) -> Result<serde_json::Value, String> + Send + Sync + 'static,
    ) -> (Calls, Entity<crate::chat_type_view::ChatTypeView>) {
        let calls: Calls = Arc::default();
        let sink = calls.clone();
        let page = shell.update(cx, |shell, cx| {
            shell.pr_review_script = Some(Arc::new(move |params| {
                sink.lock().unwrap().push(params.clone());
                answer(params)
            }));
            shell.open_pr_review_chats(cx);
            shell.ensure_pr_review_chats_page(cx)
        });
        page.update(cx, |page, cx| {
            page.set_review_prs(vec![item(7, "Add X"), item(8, "Fix Y")], cx)
        });
        draw(cx);
        (calls, page)
    }

    #[gpui::test]
    fn review_requested_rows_render_and_review_starts_once_per_request(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let (calls, _page) = open_page(&shell, cx, |_| Ok(reply("chat-review", 2)));
        for selector in [
            "pr-review-requested-row-0",
            "pr-review-requested-review-0",
            "pr-review-requested-fix-ci-0",
            "pr-review-requested-row-1",
            "pr-review-url-input",
            "pr-review-url-submit",
        ] {
            assert!(cx.debug_bounds(selector).is_some(), "{selector} renders");
        }

        click(cx, "pr-review-requested-review-0");
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
        click(cx, "pr-review-requested-review-0");
        shell.update(cx, |shell, cx| shell.open_pr_review_chats(cx));
        draw(cx);
        click(cx, "pr-review-requested-fix-ci-0");
        let calls = calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[1]["operationId"], op.as_str());
        assert_eq!(calls[2]["action"], "fixCi");
        assert_ne!(calls[2]["operationId"], op.as_str());
    }

    #[gpui::test]
    fn needs_my_review_rows_offer_review_and_open_the_new_chat(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let calls: Calls = Arc::default();
        let sink = calls.clone();
        shell.update(cx, |shell, cx| {
            shell.pr_review_script = Some(Arc::new(move |params| {
                sink.lock().unwrap().push(params.clone());
                Ok(reply("chat-sidebar", 0))
            }));
            shell.toggle_my_prs(cx);
            let page = shell.ensure_overview_page(cx);
            page.update(cx, |overview, cx| {
                overview.set_review_prs_for_test(vec![item(7, "Add X")], cx)
            });
        });
        draw(cx);
        click(cx, "overview-pr-item-review-0");
        let calls = calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["number"], 7);
        assert_eq!(calls[0]["action"], "codeReview");
        shell.read_with(cx, |shell, cx| {
            assert!(matches!(shell.route, Route::Chat));
            assert_eq!(
                shell.state.read(cx).selected_chat.as_deref(),
                Some("chat-sidebar")
            );
        });
    }

    #[gpui::test]
    fn refusals_stay_inline_on_the_row(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (shell, cx) = test_shell(cx, dir.path());
        let (_calls, page) = open_page(&shell, cx, |_| {
            Err("pr-review/fork: PR 8 comes from a fork; check it out by hand".into())
        });
        click(cx, "pr-review-requested-review-1");
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
        assert!(cx.debug_bounds("pr-review-requested-status-1").is_some());
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
}
