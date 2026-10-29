//! Overview with a busy profile: 250 chats in 25 repos, one Space whose
//! outline has 40 work items each with a PR and a Linear child. Rebuild cost
//! of `rows()`, then List and Outline paints.

use super::*;
use crate::perf::{measure, measure_n};
use crate::work_outline::store::{FIXTURE_NOW, fixture_actor, fixture_assignment, fixture_item};
use mast_proto::{
    WorkItemChatAssignmentState as AssignState, WorkItemExternalReferenceView, WorkItemStatus,
};
use std::collections::HashMap;

const SPACE: &str = "space-1";

fn chat_row(index: usize) -> mast_proto::Chat {
    use chrono::TimeZone;
    mast_proto::Chat {
        id: format!("c{index}"),
        device_id: "local".into(),
        title: Some(format!(
            "Chat {index}: investigate the failing registry sync"
        )),
        archived: false,
        cwd: Some(format!("/work/repo{}", index % 25)),
        branch: Some(format!("feature/thing-{}", index % 40)),
        checkout_id: None,
        source_context: None,
        config: None,
        last_message_preview: Some(format!("Last message preview text for chat {index}")),
        last_message_at: Some(
            Utc.timestamp_opt(1_700_000_000 + index as i64 * 600, 0)
                .unwrap(),
        ),
        created_at: Utc.timestamp_opt(1_600_000_000, 0).unwrap(),
        harness_session_id: None,
        harness_session_cwd: None,
        actual_model: None,
        parent_chat_id: None,
        space_id: Some(SPACE.into()),
        last_seen_at: None,
        room_gen: None,
        linked_pr_url: None,
        linked_pr_source: None,
        linked_ticket_id: None,
        linked_ticket_source: None,
    }
}

fn snapshot() -> WorkOutlineSnapshot {
    let mut snap = crate::work_outline::store::fixture_snapshot(SPACE);
    for i in 0..40 {
        let root = format!("w-bench-{i}");
        let pr = format!("w-bench-{i}-pr");
        let lin = format!("w-bench-{i}-lin");
        let mut items = vec![
            fixture_item(
                &root,
                SPACE,
                None,
                &format!("b{i:03}"),
                &format!("Ticket {i}: ship the thing"),
                WorkItemStatus::Active,
                &[&pr, &lin],
                &[&format!("c{}", i * 5)],
            ),
            fixture_item(
                &pr,
                SPACE,
                Some(&root),
                "a0",
                &format!("PR review: acme/repo#{i}"),
                WorkItemStatus::Active,
                &[],
                &[&format!("c{}", i * 5 + 1)],
            ),
            fixture_item(
                &lin,
                SPACE,
                Some(&root),
                "a1",
                &format!("ENG-{i}: linked issue"),
                WorkItemStatus::Pending,
                &[],
                &[&format!("c{}", i * 5 + 2)],
            ),
        ];
        items[1].depth = 1;
        items[2].depth = 1;
        for (item, provider, kind, ext) in [
            (&pr, "github", "pullRequest", format!("acme/repo#{i}")),
            (&lin, "linear", "issue", format!("ENG-{i}")),
        ] {
            snap.external_references
                .push(WorkItemExternalReferenceView {
                    id: format!("ref-{item}"),
                    space_id: SPACE.into(),
                    work_item_id: item.clone(),
                    provider: provider.into(),
                    reference_type: kind.into(),
                    external_id: ext.clone(),
                    url: format!("https://example.com/{ext}"),
                    cached_title: Some(format!("Title of {ext}")),
                    created_by: Some(fixture_actor()),
                    created_at: FIXTURE_NOW,
                });
        }
        for (n, work_item) in [&root, &pr, &lin].into_iter().enumerate() {
            snap.assignments.push(fixture_assignment(
                &format!("c{}", i * 5 + n),
                SPACE,
                Some(work_item),
                AssignState::Associated,
            ));
        }
        snap.root_work_item_ids.push(root);
        snap.items.append(&mut items);
    }
    snap
}

#[gpui::test]
#[ignore = "benchmark: run with --ignored --nocapture"]
fn bench_overview_250_chats_40_items(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::settings::init(Default::default(), dir.path(), cx);
    });
    let state = cx.new(|_| AppState::new());
    state.update(cx, |state, _| {
        state.data_dir = Some(dir.path().to_path_buf());
        state.chats = (0..250).map(chat_row).collect();
        state.spaces = vec![mast_proto::Space {
            id: SPACE.into(),
            device_id: "local".into(),
            path: "/work".into(),
            name: Some("Bench".into()),
            git_detected: true,
            git_checked_at: None,
            checkout_id: None,
            created_at: Utc::now(),
        }];
    });
    let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
    let composer = cx.new(|cx| crate::composer::Composer::new(state.clone(), cx));
    let st = state.clone();
    let (overview, vcx) = cx.add_window_view(move |_, cx| {
        Overview::new(
            st,
            gpui::WeakEntity::new_invalid(),
            transcript,
            composer,
            cx,
        )
    });
    vcx.run_until_parked();
    let draw = |vcx: &mut gpui::VisualTestContext| {
        vcx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear();
        });
    };
    overview.update(vcx, |o, _| {
        let mut map = HashMap::new();
        map.insert(
            SPACE.to_string(),
            WorkOutlineLoad::Ready(std::sync::Arc::new(snapshot())),
        );
        o.outline_fixture = Some(map);
        o.view_mode = OverviewMode::List;
        o.rows_dirty = true;
    });
    measure("overview", "first draw (List)", || draw(vcx));
    measure_n("overview", "rows() forced rebuild", 20, |_| {
        overview.update(vcx, |o, cx| {
            o.rows_dirty = true;
            std::hint::black_box(o.rows(cx));
        });
    });
    measure_n("overview", "rows() cached", 200, |_| {
        overview.update(vcx, |o, cx| {
            std::hint::black_box(o.rows(cx));
        });
    });
    measure_n("overview", "List redraw (clean)", 30, |_| draw(vcx));
    measure_n("overview", "List redraw (rows dirty)", 20, |_| {
        overview.update(vcx, |o, _| o.rows_dirty = true);
        draw(vcx);
    });
    overview.update(vcx, |o, cx| {
        o.view_mode = OverviewMode::Outline;
        cx.notify();
    });
    measure("overview", "first draw (Outline)", || draw(vcx));
    measure_n("overview", "Outline redraw (clean)", 30, |_| draw(vcx));
    measure_n("overview", "Outline redraw (rows dirty)", 20, |_| {
        overview.update(vcx, |o, _| o.rows_dirty = true);
        draw(vcx);
    });
}
