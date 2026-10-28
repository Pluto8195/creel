//! "Diff branch / PR…" in the Changes pane, headless: the picker (linked PRs,
//! ticket branches, free text), the pills ("PR #n" / "branch <name>"), the
//! range scope (no Working tree or Latest turn) and the refs-only capture.

use super::*;
use std::sync::{Arc, Mutex};
use mast_proto::{
    ChatDiffTargetsView, ChatRefTargetView, DiffFileSummary, RefDiffCandidate, RefDiffKind,
};

const FLAGSHIP: &str = "/work/ws/flagship";
const FLAGSHIP_CHECKOUT: &str = "checkout:flagship";
const PR_URL: &str = "https://github.com/cofactr/flagship/pull/6487";
const PR2_URL: &str = "https://github.com/cofactr/flagship/pull/6488";
const TICKET_BRANCH: &str = "michael/eng-3012-kit-line";

const PATCH: &str = "diff --git a/feature.txt b/feature.txt\n\
new file mode 100644\n\
index 0000000..e69de29\n\
--- /dev/null\n\
+++ b/feature.txt\n\
@@ -0,0 +1 @@\n\
+kit line\n";

fn candidate(
    kind: RefDiffKind,
    label: &str,
    input: &str,
    linked: bool,
    path: &str,
) -> RefDiffCandidate {
    RefDiffCandidate {
        kind,
        label: label.into(),
        detail: input.into(),
        input: input.into(),
        path: path.into(),
        repo_name: "flagship".into(),
        linked,
        suggested: !linked,
        bound: false,
    }
}

/// The engine's answer: linked PRs first, then the ticket's branch.
fn candidates() -> Vec<RefDiffCandidate> {
    vec![
        candidate(RefDiffKind::Pr, "PR #6487", PR_URL, true, FLAGSHIP),
        candidate(RefDiffKind::Pr, "PR #6488", PR2_URL, true, FLAGSHIP),
        candidate(
            RefDiffKind::Branch,
            &format!("branch {TICKET_BRANCH}"),
            TICKET_BRANCH,
            false,
            FLAGSHIP,
        ),
    ]
}

fn ref_view(id: &str, label: &str, head_branch: &str, selected: bool) -> ChatRefTargetView {
    ChatRefTargetView {
        id: id.into(),
        kind: if label.starts_with("PR") {
            RefDiffKind::Pr
        } else {
            RefDiffKind::Branch
        },
        label: label.into(),
        path: FLAGSHIP.into(),
        checkout_id: FLAGSHIP_CHECKOUT.into(),
        repo_name: "flagship".into(),
        head_branch: head_branch.into(),
        head_ref: format!("refs/remotes/origin/{head_branch}"),
        base_branch: "main".into(),
        base_ref: "refs/remotes/origin/main".into(),
        head_sha: Some("4fb769a2ff26f181dd5d0c2446a8b20702e4f2ce".into()),
        pr_number: label.strip_prefix("PR #").and_then(|n| n.parse().ok()),
        pr_url: None,
        selected,
    }
}

fn ref_frame(refs: Vec<ChatRefTargetView>) -> Vec<ChatDiffTargetsView> {
    vec![ChatDiffTargetsView {
        chat_id: "chat-wt".into(),
        targets: Vec::new(),
        ref_targets: refs,
    }]
}

fn two_refs(pr_selected: bool, branch_selected: bool) -> Vec<ChatRefTargetView> {
    vec![
        ref_view("pr-1", "PR #6487", TICKET_BRANCH, pr_selected),
        ref_view(
            "br-1",
            "branch michael/eng-3012-part-2",
            "michael/eng-3012-part-2",
            branch_selected,
        ),
    ]
}

/// The diff-target RPCs of the fake engine (answered at once through
/// `AppState`'s test transport); every call is recorded.
fn target_rpc(calls: RpcCalls, bind_error: Option<&'static str>) -> crate::diff_targets::FakeRpc {
    Arc::new(move |method, params| {
        calls.lock().unwrap().push((method.to_owned(), params));
        match method {
            mast_rpc::methods::LIST_CHAT_REF_DIFF_CANDIDATES => Ok(serde_json::to_value(
                mast_proto::ListChatRefDiffCandidatesReply {
                    candidates: candidates(),
                },
            )
            .unwrap()),
            mast_rpc::methods::SET_CHAT_DIFF_TARGET => match bind_error {
                Some(message) => Err(message.to_owned()),
                None => {
                    Ok(serde_json::to_value(mast_proto::ChatDiffTargetsReply::default()).unwrap())
                }
            },
            other => Err(format!("unexpected {other}")),
        }
    })
}

struct Opened<'a> {
    changes: Entity<Changes>,
    state: Entity<AppState>,
    /// Diff-target RPCs the pane made.
    calls: RpcCalls,
    /// Frames the pane sent to the engine itself (diff watch, scoped
    /// capture, branch list): what the client wrote to its transport.
    frames: tokio::sync::mpsc::Receiver<String>,
    vcx: &'a mut gpui::VisualTestContext,
}

impl Opened<'_> {
    /// Every engine request sent so far as `(method, params)`.
    fn engine_requests(&mut self) -> Vec<(String, serde_json::Value)> {
        let mut out = Vec::new();
        while let Ok(frame) = self.frames.try_recv() {
            let value: serde_json::Value = serde_json::from_str(&frame).unwrap();
            if let Some(method) = value["method"].as_str() {
                out.push((method.to_owned(), value["params"].clone()));
            }
        }
        out
    }
}

/// The pane the way the shell hosts it: its header controls (scope title,
/// ref pair) above the body.
struct Host {
    changes: Entity<Changes>,
}

impl gpui::Render for Host {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let controls = self
            .changes
            .update(cx, |changes, cx| changes.render_header_controls(cx));
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(div().flex_none().h(px(32.0)).child(controls))
            .child(div().flex_1().min_h_0().child(self.changes.clone()))
    }
}

/// The tests need a tokio reactor for the RPC client's reader.
fn open<'a>(
    cx: &'a mut gpui::TestAppContext,
    frame: Option<Vec<ChatDiffTargetsView>>,
    bind_error: Option<&'static str>,
) -> Opened<'a> {
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::dark());
    });
    let calls: RpcCalls = Default::default();
    let (out, frames) = tokio::sync::mpsc::channel::<String>(64);
    let (server_out, inbound) = tokio::sync::mpsc::channel::<String>(64);
    // The client's reader must never see its inbound side close: it would
    // fail the pane's pending calls from the reactor thread, which gpui's
    // test scheduler rejects. Leaking the sender keeps it open.
    std::mem::forget(server_out);
    let client = mast_rpc::RpcClient::new(out, inbound);
    let state_slot: Arc<Mutex<Option<Entity<AppState>>>> = Default::default();
    let slot = state_slot.clone();
    let rpc = target_rpc(calls.clone(), bind_error);
    let (host, vcx) = cx.add_window_view(|_, cx| {
        let mut state = AppState::new();
        state.local_device_id = Some("d".into());
        state.chats = vec![wt_chat()];
        state.set_test_engine(crate::state::EngineHandle::from_test_client(client));
        state.chat_diff_rpc_override = Some(rpc);
        if let Some(frame) = frame {
            state.apply_chat_diff_targets(frame);
        }
        let state = cx.new(|_| state);
        *slot.lock().unwrap() = Some(state.clone());
        let changes = cx.new(|cx| Changes::for_chat(state, "chat-wt".into(), cx));
        Host { changes }
    });
    let changes = host.read_with(vcx, |host, _| host.changes.clone());
    // The shell calls this when it shows the pane (it starts the watch and
    // any scoped capture).
    changes.update(vcx, |changes, cx| changes.ensure_content(cx));
    vcx.run_until_parked();
    draw(vcx);
    let state = state_slot.lock().unwrap().clone().unwrap();
    Opened {
        changes,
        state,
        calls,
        frames,
        vcx,
    }
}

/// Run what the pane queued (the diff-target transport answers at once).
fn settle(
    vcx: &mut gpui::VisualTestContext,
    mut done: impl FnMut(&mut gpui::VisualTestContext) -> bool,
) {
    for _ in 0..50 {
        vcx.run_until_parked();
        if done(vcx) {
            return;
        }
    }
    panic!("condition not reached");
}

fn draw(vcx: &mut gpui::VisualTestContext) {
    vcx.update(|window, cx| window.draw(cx).clear());
}

fn key(key: &str) -> gpui::KeyDownEvent {
    gpui::KeyDownEvent {
        keystroke: gpui::Keystroke::parse(key).unwrap(),
        is_held: false,
        prefer_character_input: false,
    }
}

fn open_ref_picker(changes: &Entity<Changes>, calls: &RpcCalls, vcx: &mut gpui::VisualTestContext) {
    vcx.update(|window, cx| {
        changes.update(cx, |changes, cx| changes.open_ref_target_menu(window, cx))
    });
    settle(vcx, |_| {
        !rpc_calls(calls, mast_rpc::methods::LIST_CHAT_REF_DIFF_CANDIDATES).is_empty()
    });
    vcx.run_until_parked();
}

fn type_query(changes: &Entity<Changes>, vcx: &mut gpui::VisualTestContext, text: &str) {
    let text = text.to_owned();
    changes.update(vcx, |changes, cx| {
        let search = changes.ref_target_menu.get().unwrap().search.clone();
        search.update(cx, |input, cx| input.set_text(text, cx));
    });
    vcx.run_until_parked();
}

fn rows(changes: &Entity<Changes>, vcx: &mut gpui::VisualTestContext) -> Vec<RefRow> {
    changes.read_with(vcx, |changes, cx| changes.ref_target_rows(cx))
}

use crate::diff_targets::RefRow;

// ── the picker ──────────────────────────────────────────────────────────────

#[gpui::test]
fn the_picker_sits_next_to_diff_checkout_and_lists_linked_prs_before_ticket_branches(
    cx: &mut gpui::TestAppContext,
) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _guard = runtime.enter();
    let Opened {
        changes,
        calls,
        vcx,
        frames: _frames,
        ..
    } = open(cx, None, None);
    draw(vcx);
    let checkout = vcx
        .debug_bounds("changes-checkout-trigger")
        .expect("Diff checkout…");
    let branch_or_pr = vcx
        .debug_bounds("changes-ref-target-trigger")
        .expect("Diff branch / PR…");
    // Same chrome row, side by side.
    assert_eq!(checkout.center().y, branch_or_pr.center().y);
    assert!(branch_or_pr.right() <= checkout.left() + px(1.0));

    open_ref_picker(&changes, &calls, vcx);
    assert_eq!(
        rpc_calls(&calls, mast_rpc::methods::LIST_CHAT_REF_DIFF_CANDIDATES),
        vec![serde_json::json!({"chatId": "chat-wt"})]
    );
    assert_eq!(
        rows(&changes, vcx),
        vec![
            RefRow::Candidate(0),
            RefRow::Candidate(1),
            RefRow::Candidate(2)
        ]
    );
    draw(vcx);
    assert!(vcx.debug_bounds("changes-ref-target-row-0").is_some());
    assert!(vcx.debug_bounds("changes-ref-target-row-2").is_some());
    // Escape closes it first.
    changes.update(vcx, |changes, cx| assert!(changes.handle_escape(cx)));
    vcx.run_until_parked();
    changes.read_with(vcx, |changes, _| {
        assert!(!changes.ref_target_menu.is_open())
    });
}

#[gpui::test]
fn choosing_a_row_binds_it_through_the_engine(cx: &mut gpui::TestAppContext) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _guard = runtime.enter();
    let Opened {
        changes,
        state,
        calls,
        vcx,
        frames: _frames,
        ..
    } = open(cx, None, None);
    open_ref_picker(&changes, &calls, vcx);
    draw(vcx);
    // A real click on the first row: the linked PR (by URL, no repo hint).
    let row = vcx
        .debug_bounds("changes-ref-target-row-0")
        .unwrap()
        .center();
    vcx.simulate_click(row, gpui::Modifiers::default());
    settle(vcx, |_| {
        !rpc_calls(&calls, mast_rpc::methods::SET_CHAT_DIFF_TARGET).is_empty()
    });
    assert_eq!(
        rpc_calls(&calls, mast_rpc::methods::SET_CHAT_DIFF_TARGET),
        vec![serde_json::json!({
            "chatId": "chat-wt", "action": "bindRef", "input": PR_URL,
        })]
    );
    settle(vcx, |vcx| {
        changes.read_with(vcx, |changes, _| !changes.ref_target_menu.is_open())
    });
    state.read_with(vcx, |state, _| {
        assert_eq!(state.chat_diff_target_error("chat-wt"), None);
    });

    // A branch candidate carries the repository it was listed in.
    open_ref_picker(&changes, &calls, vcx);
    changes.update(vcx, |changes, cx| {
        changes.ref_target_menu_key(&key("down"), cx);
        changes.ref_target_menu_key(&key("down"), cx);
        changes.ref_target_menu_key(&key("enter"), cx);
    });
    settle(vcx, |_| {
        rpc_calls(&calls, mast_rpc::methods::SET_CHAT_DIFF_TARGET).len() == 2
    });
    assert_eq!(
        rpc_calls(&calls, mast_rpc::methods::SET_CHAT_DIFF_TARGET)[1],
        serde_json::json!({
            "chatId": "chat-wt", "action": "bindRef",
            "input": TICKET_BRANCH, "repoPath": FLAGSHIP,
        })
    );
}

#[gpui::test]
fn free_text_accepts_a_pr_number_a_pr_url_and_a_branch_name(cx: &mut gpui::TestAppContext) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _guard = runtime.enter();
    let Opened {
        changes,
        calls,
        vcx,
        frames: _frames,
        ..
    } = open(cx, None, None);
    open_ref_picker(&changes, &calls, vcx);

    // A PR that is not linked: offered as typed.
    type_query(&changes, vcx, "7000");
    let typed = rows(&changes, vcx);
    assert_eq!(typed.len(), 1);
    assert!(matches!(&typed[0], RefRow::FreeText(f) if f.label == "PR #7000" && f.input == "7000"));
    draw(vcx);
    assert!(vcx.debug_bounds("changes-ref-target-row-0").is_some());

    // A linked PR typed by number is the candidate, not a duplicate.
    type_query(&changes, vcx, "#6488");
    assert_eq!(rows(&changes, vcx), vec![RefRow::Candidate(1)]);

    // A PR URL that is not linked.
    type_query(
        &changes,
        vcx,
        "https://github.com/cofactr/flagship/pull/7001",
    );
    let typed = rows(&changes, vcx);
    assert!(matches!(&typed[0], RefRow::FreeText(f) if f.label == "PR #7001"));

    // A branch name: no hint (the engine looks for it).
    type_query(&changes, vcx, "origin/feature/x");
    changes.update(vcx, |changes, cx| {
        changes.ref_target_menu_key(&key("enter"), cx)
    });
    settle(vcx, |_| {
        !rpc_calls(&calls, mast_rpc::methods::SET_CHAT_DIFF_TARGET).is_empty()
    });
    assert_eq!(
        rpc_calls(&calls, mast_rpc::methods::SET_CHAT_DIFF_TARGET),
        vec![serde_json::json!({
            "chatId": "chat-wt", "action": "bindRef", "input": "origin/feature/x",
        })]
    );
}

#[gpui::test]
fn a_refused_bind_shows_the_engines_message_and_closes_the_picker(cx: &mut gpui::TestAppContext) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _guard = runtime.enter();
    let message = "Branch nope was not found in ws, flagship (git fetch said: not found).";
    let Opened {
        changes,
        state,
        calls,
        vcx,
        frames: _frames,
        ..
    } = open(cx, None, Some(message));
    open_ref_picker(&changes, &calls, vcx);
    type_query(&changes, vcx, "nope");
    changes.update(vcx, |changes, cx| {
        changes.ref_target_menu_key(&key("enter"), cx)
    });
    settle(vcx, |vcx| {
        state.read_with(vcx, |state, _| {
            state.chat_diff_target_error("chat-wt").is_some()
        })
    });
    state.read_with(vcx, |state, _| {
        assert!(
            state
                .chat_diff_target_error("chat-wt")
                .unwrap()
                .contains("Branch nope was not found")
        );
    });
    settle(vcx, |vcx| {
        changes.read_with(vcx, |changes, _| !changes.ref_target_menu.is_open())
    });
    draw(vcx);
    assert!(vcx.debug_bounds("changes-diff-target-error").is_some());
}

// ── pills and scope ─────────────────────────────────────────────────────────

#[gpui::test]
fn pills_are_labelled_pr_n_and_branch_name_beside_the_own_checkout(cx: &mut gpui::TestAppContext) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _guard = runtime.enter();
    let Opened {
        state,
        vcx,
        frames: _frames,
        ..
    } = open(cx, Some(ref_frame(two_refs(true, false))), None);
    state.read_with(vcx, |state, _| {
        let labels: Vec<String> = state
            .chat_execution_members("chat-wt")
            .iter()
            .map(|member| member.label())
            .collect();
        // Own checkout first, then the ref targets in stored order.
        assert_eq!(labels.len(), 3);
        assert!(labels[0].contains("main"), "{labels:?}");
        assert_eq!(labels[1], "PR #6487");
        assert_eq!(labels[2], "branch michael/eng-3012-part-2");
        let selected = state.selected_execution_member("chat-wt").unwrap();
        assert_eq!(selected.label(), "PR #6487");
        assert_eq!(
            selected
                .range
                .as_ref()
                .map(|r| (r.base_branch.as_str(), r.head_branch.as_str())),
            Some(("main", TICKET_BRANCH))
        );
    });
}

#[gpui::test]
fn a_selected_ref_target_puts_the_pane_in_range_scope_with_no_working_tree_or_turn(
    cx: &mut gpui::TestAppContext,
) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _guard = runtime.enter();
    let Opened {
        changes,
        state,
        vcx,
        frames: _frames,
        ..
    } = open(cx, None, None);
    // Before: the chat's own checkout, the normal scopes.
    draw(vcx);
    changes.read_with(vcx, |changes, _| {
        assert_eq!(changes.scope, DiffScope::WorkingTree);
        assert!(changes.owner.as_ref().unwrap().range.is_none());
    });
    assert!(vcx.debug_bounds("changes-scope-trigger").is_some());
    assert!(vcx.debug_bounds("changes-range-title").is_none());

    // The engine reports a selected PR target.
    state.update(vcx, |state, cx| {
        state.apply_chat_diff_targets(ref_frame(two_refs(true, false)));
        cx.notify();
    });
    vcx.run_until_parked();
    draw(vcx);
    changes.read_with(vcx, |changes, _| {
        assert_eq!(changes.scope, DiffScope::Range);
        let owner = changes.owner.as_ref().unwrap();
        assert_eq!(owner.cwd.as_deref(), Some(FLAGSHIP));
        assert_eq!(owner.checkout_id.as_deref(), Some(FLAGSHIP_CHECKOUT));
        assert!(owner.exact_checkout);
        let range = owner.range.as_ref().unwrap();
        assert_eq!(range.label, "PR #6487");
        assert_eq!(range.base_ref, "refs/remotes/origin/main");
        assert_eq!(
            range.head_ref,
            format!("refs/remotes/origin/{TICKET_BRANCH}")
        );
    });
    // Working tree and Latest turn are gone: no scope menu, a fixed title.
    assert!(vcx.debug_bounds("changes-scope-trigger").is_none());
    assert!(vcx.debug_bounds("changes-range-title").is_some());
    assert!(vcx.debug_bounds("changes-range-refs").is_some());
    assert!(!DiffScope::ALL.contains(&DiffScope::Range));
    // The pills: own checkout, PR #6487 (active), branch.
    assert!(vcx.debug_bounds("changes-execution-repo-2").is_some());

    // Picking the chat's own checkout pill restores the normal scopes.
    let own = crate::work_outline::execution::own_member_id("chat-wt");
    state.update(vcx, |state, cx| {
        state.select_chat_execution_member("chat-wt", &own, cx)
    });
    vcx.run_until_parked();
    draw(vcx);
    changes.read_with(vcx, |changes, _| {
        assert_eq!(changes.scope, DiffScope::WorkingTree);
        assert!(changes.owner.as_ref().unwrap().range.is_none());
    });
    assert!(vcx.debug_bounds("changes-scope-trigger").is_some());
    assert!(vcx.debug_bounds("changes-range-title").is_none());
}

#[gpui::test]
fn picking_a_ref_pill_persists_select_ref_and_a_vanished_ref_drops_the_pick(
    cx: &mut gpui::TestAppContext,
) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _guard = runtime.enter();
    let Opened {
        state,
        calls,
        vcx,
        frames: _frames,
        ..
    } = open(cx, Some(ref_frame(two_refs(true, false))), None);
    // The second pill is the branch target.
    state.update(vcx, |state, cx| {
        state.select_chat_execution_member("chat-wt", "ref:br-1", cx)
    });
    settle(vcx, |_| {
        !rpc_calls(&calls, mast_rpc::methods::SET_CHAT_DIFF_TARGET).is_empty()
    });
    assert_eq!(
        rpc_calls(&calls, mast_rpc::methods::SET_CHAT_DIFF_TARGET),
        vec![serde_json::json!({
            "chatId": "chat-wt", "action": "selectRef", "refId": "br-1",
        })]
    );
    state.read_with(vcx, |state, _| {
        assert_eq!(
            state.selected_execution_member("chat-wt").unwrap().label(),
            "branch michael/eng-3012-part-2"
        );
    });
    // The engine drops that ref target: the pick goes with it.
    state.update(vcx, |state, _| {
        state.apply_chat_diff_targets(ref_frame(vec![ref_view(
            "pr-1",
            "PR #6487",
            TICKET_BRANCH,
            true,
        )]));
    });
    state.read_with(vcx, |state, _| {
        assert_eq!(
            state.selected_execution_member("chat-wt").unwrap().label(),
            "PR #6487"
        );
    });
}

// ── the capture ─────────────────────────────────────────────────────────────

#[gpui::test]
fn the_range_capture_sends_both_refs_only_and_shows_the_files(cx: &mut gpui::TestAppContext) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _guard = runtime.enter();
    let mut opened = open(cx, Some(ref_frame(two_refs(true, false))), None);
    let requests = opened.engine_requests();
    let diffs: Vec<_> = requests
        .iter()
        .filter(|(method, _)| method == mast_rpc::methods::GET_CHECKOUT_DIFF)
        .collect();
    assert_eq!(diffs.len(), 1, "{requests:?}");
    // Exactly the ref pair of the target (the chat id rides along on every
    // scoped capture; the engine ignores it for a range): no commit, no
    // working tree base, no turn.
    assert_eq!(
        diffs[0].1,
        serde_json::json!({
            "cwd": FLAGSHIP,
            "checkoutId": FLAGSHIP_CHECKOUT,
            "mode": "range",
            "chatId": "chat-wt",
            "baseRef": "refs/remotes/origin/main",
            "headRef": format!("refs/remotes/origin/{TICKET_BRANCH}"),
        })
    );
    // The branch list (the Branch scope's base picker) is never requested.
    assert!(
        !requests
            .iter()
            .any(|(method, _)| method == mast_rpc::methods::LIST_BRANCHES),
        "{requests:?}"
    );

    // The engine's answer fills the same file list and diff view as the
    // Branch scope (its reply is applied the way the capture task does).
    let diff = CheckoutDiff {
        checkout_id: FLAGSHIP_CHECKOUT.into(),
        device_id: "d".into(),
        cwd: FLAGSHIP.into(),
        patch: PATCH.into(),
        files: vec![DiffFileSummary {
            path: "feature.txt".into(),
            old_path: None,
            status: "added".into(),
            additions: 1,
            deletions: 0,
            binary: false,
        }],
        submodules: Vec::new(),
        additions: 1,
        deletions: 0,
        truncated: false,
        checksum: "range-sum".into(),
        updated_at: Utc::now(),
    };
    let vcx = &mut *opened.vcx;
    opened.changes.update(vcx, |changes, cx| {
        changes.scoped_for = changes.scoped_inflight.take();
        changes.scoped = Some(diff);
        changes.sync(cx);
    });
    for _ in 0..50 {
        vcx.run_until_parked();
        if opened
            .changes
            .read_with(vcx, |changes, _| changes.parsed.is_some())
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    opened.changes.read_with(vcx, |changes, _| {
        let parsed = changes.parsed.as_ref().expect("diff parsed");
        assert_eq!(parsed.files.len(), 1);
        assert_eq!(parsed.files[0].path, "feature.txt");
    });
    assert_eq!(
        scope_label(DiffScope::Range, 1, Some("main")),
        "1 Changed file vs main"
    );
    assert_eq!(
        clean_message(DiffScope::Range, Some("main")),
        "No changes vs main"
    );
}

#[gpui::test]
fn chats_without_ref_targets_are_unchanged(cx: &mut gpui::TestAppContext) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _guard = runtime.enter();
    let mut opened = open(cx, None, None);
    let requests = opened.engine_requests();
    let Opened {
        changes,
        state,
        calls,
        vcx,
        frames: _frames,
        ..
    } = opened;
    draw(vcx);
    state.read_with(vcx, |state, _| {
        assert!(state.chat_execution_members("chat-wt").is_empty());
        assert!(state.chat_ref_targets("chat-wt").is_empty());
        assert!(state.selected_execution_member("chat-wt").is_none());
    });
    changes.read_with(vcx, |changes, _| {
        assert_eq!(changes.scope, DiffScope::WorkingTree);
        let owner = changes.owner.as_ref().unwrap();
        assert!(owner.range.is_none());
        assert_eq!(owner.cwd.as_deref(), Some(WS));
        assert!(changes.scoped.is_none());
    });
    assert!(vcx.debug_bounds("changes-scope-trigger").is_some());
    assert!(vcx.debug_bounds("changes-range-title").is_none());
    assert!(vcx.debug_bounds("changes-execution-repositories").is_none());
    // The ordinary watch, and no range capture.
    assert!(
        !requests
            .iter()
            .any(|(method, _)| method == mast_rpc::methods::GET_CHECKOUT_DIFF),
        "{requests:?}"
    );
    // Opening and closing the picker writes nothing.
    open_ref_picker(&changes, &calls, vcx);
    changes.update(vcx, |changes, cx| changes.close_ref_target_menu(cx));
    assert!(rpc_calls(&calls, mast_rpc::methods::SET_CHAT_DIFF_TARGET).is_empty());
}
