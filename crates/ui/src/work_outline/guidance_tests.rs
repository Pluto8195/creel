// First-use empty state of the Outline. Included from `overview.rs`'s test
// module so it shares its Overview fixtures.
use super::*;

fn empty_snapshot() -> WorkOutlineSnapshot {
    WorkOutlineSnapshot {
        space_id: "s1".into(),
        available: true,
        synced: true,
        ..Default::default()
    }
}

fn first_use_shown(vcx: &mut gpui::VisualTestContext) -> bool {
    vcx.debug_bounds("outline-first-use").is_some()
}

#[gpui::test]
fn first_use_line_shows_only_without_work_items_and_creates_one(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (overview, calls, vcx) = instruction_overview(cx, dir.path(), empty_snapshot(), reply_ok);
    let outline = overview.read_with(vcx, |o, _| o.outline.clone());
    draw(vcx);
    assert!(
        first_use_shown(vcx),
        "no work items: the first-use line shows"
    );
    assert!(vcx.debug_bounds("outline-first-use-learn-more").is_some());

    // "Create work item" starts the inline create; the line steps aside.
    let create = vcx
        .debug_bounds("outline-first-use-create")
        .expect("create action")
        .center();
    vcx.simulate_click(create, gpui::Modifiers::default());
    draw(vcx);
    outline.read_with(vcx, |o, _| assert!(o.is_editing()));
    assert!(
        !first_use_shown(vcx),
        "hidden while the item is being created"
    );
    outline.update(vcx, |o, cx| o.commit_title_for_test("Launch v2", cx));
    vcx.run_until_parked();
    let call = calls.lock().unwrap().last().cloned().expect("a mutation");
    assert_eq!(call["op"], "createWorkItem", "{call}");
    assert_eq!(call["title"], "Launch v2", "{call}");

    // As soon as an item exists the line is gone.
    overview.update(vcx, |o, cx| {
        o.set_outline_loads(
            fixture_loads(Some(crate::work_outline::store::fixture_snapshot("s1"))),
            cx,
        )
    });
    draw(vcx);
    assert!(!first_use_shown(vcx), "never once work items exist");

    // Back to zero items: it returns, but not while searching. (The created
    // item shows optimistically until a frame has it or the wait expires.)
    vcx.executor().advance_clock(std::time::Duration::from_secs(3));
    vcx.run_until_parked();
    overview.update(vcx, |o, cx| {
        o.set_outline_loads(fixture_loads(Some(empty_snapshot())), cx)
    });
    draw(vcx);
    assert!(first_use_shown(vcx));
    overview.update(vcx, |o, cx| {
        o.search
            .update(cx, |search, cx| search.set_text("chat", cx));
        o.outline_dirty = true;
        cx.notify();
    });
    draw(vcx);
    assert!(!first_use_shown(vcx), "not while searching");
}
