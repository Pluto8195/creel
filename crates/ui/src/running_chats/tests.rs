use super::*;
use chrono::TimeZone;

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 29, 12, 0, 0).unwrap()
}

fn input(id: &str, space: &str, state: RunState, mins_ago: i64) -> Input {
    Input {
        chat_id: id.into(),
        parent_id: None,
        space_id: Some(space.into()),
        title: id.into(),
        state,
        since: Some(now() - chrono::Duration::minutes(mins_ago)),
        activity: String::new(),
        item_path: None,
        account: None,
        model: None,
        background: 0,
    }
}

fn spaces() -> Vec<(String, String)> {
    vec![
        ("m".into(), "Mast".into()),
        ("c".into(), "Cofactr".into()),
        ("p".into(), "Personal".into()),
    ]
}

fn ids(snapshot: &Snapshot) -> Vec<String> {
    snapshot.entries().map(Entry::key).collect()
}

#[test]
fn zero_chats_is_an_empty_snapshot() {
    let snap = snapshot(&[], &spaces(), &Toggled::new(), now());
    assert!(snap.sections.is_empty());
    assert_eq!(snap.headline(), "Nothing running");
    assert_eq!(step(&snap, None, 1), None);
    assert_eq!(reconcile(&snap, Some("x")), None);
}

#[test]
fn three_chats_group_by_space_in_space_order_oldest_first() {
    let inputs = vec![
        input("late", "c", RunState::Working, 2),
        input("early", "c", RunState::Working, 12),
        input("mast", "m", RunState::Working, 8),
    ];
    let snap = snapshot(&inputs, &spaces(), &Toggled::new(), now());
    let labels: Vec<_> = snap.sections.iter().map(|s| s.label.as_str()).collect();
    assert_eq!(labels, ["Mast", "Cofactr"]);
    assert_eq!(ids(&snap), ["mast", "early", "late"]);
    assert_eq!(snap.headline(), "3 running");
}

#[test]
fn needs_you_comes_first_and_leaves_its_space() {
    let mut asked = input("asks", "m", RunState::Awaiting, 5);
    asked.parent_id = Some("coord".into());
    let inputs = vec![
        input("coord", "m", RunState::Working, 70),
        asked,
        input("broke", "c", RunState::Errored, 1),
    ];
    let snap = snapshot(&inputs, &spaces(), &Toggled::new(), now());
    assert_eq!(snap.sections[0].kind, SectionKind::NeedsYou);
    assert_eq!(snap.sections[0].count, 2);
    assert_eq!(ids(&snap), ["asks", "broke", "coord"]);
    assert_eq!(snap.headline(), "3 running \u{b7} 2 need you");
}

#[test]
fn children_indent_under_their_coordinator() {
    let mut kid = input("kid", "m", RunState::Working, 3);
    kid.parent_id = Some("coord".into());
    let mut orphan = input("orphan", "m", RunState::Working, 4);
    orphan.parent_id = Some("elsewhere".into());
    let inputs = vec![input("coord", "m", RunState::Working, 60), kid, orphan];
    let snap = snapshot(&inputs, &spaces(), &Toggled::new(), now());
    let depths: Vec<_> = snap
        .entries()
        .map(|e| match e {
            Entry::Row(r) => (r.chat_id.as_str(), r.depth),
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(depths, [("coord", 0), ("kid", 1), ("orphan", 0)]);
}

#[test]
fn idle_with_background_is_its_own_state_with_idle_elapsed() {
    let mut idle = input("bg", "m", RunState::IdleBackground, 6);
    idle.background = 1;
    let snap = snapshot(&[idle], &spaces(), &Toggled::new(), now());
    let Some(Entry::Row(row)) = snap.entries().next() else {
        panic!()
    };
    assert_eq!(row.state, RunState::IdleBackground);
    assert_eq!(row.elapsed, "6m idle");
    assert!(!row.state.needs_you());
}

#[test]
fn fifteen_chats_fold_children_and_late_spaces() {
    let mut inputs = vec![
        input("mc", "m", RunState::Working, 72),
        input("cc", "c", RunState::Working, 42),
    ];
    for n in 0..4 {
        let mut kid = input(&format!("mk{n}"), "m", RunState::Working, 10 + n);
        kid.parent_id = Some("mc".into());
        inputs.push(kid);
    }
    for n in 0..5 {
        let mut kid = input(&format!("ck{n}"), "c", RunState::Working, 10 + n);
        kid.parent_id = Some("cc".into());
        inputs.push(kid);
    }
    for n in 0..2 {
        inputs.push(input(&format!("p{n}"), "p", RunState::Working, 2 + n));
    }
    inputs.push(input("a", "m", RunState::Awaiting, 5));
    inputs.push(input("b", "c", RunState::Awaiting, 21));
    assert_eq!(inputs.len(), 15);
    let snap = snapshot(&inputs, &spaces(), &Toggled::new(), now());
    assert_eq!(snap.total, 15);
    assert_eq!(
        ids(&snap),
        ["b", "a", "mc", "fold:mc", "cc", "fold:cc", "fold:space:p"]
    );
    assert!(matches!(
        snap.find("fold:mc"),
        Some(Entry::Fold { hidden: 4, .. })
    ));
    assert_eq!(snap.sections.last().unwrap().count, 2);

    // Toggling unfolds just that coordinator, and the Space.
    let toggled: Toggled = ["mc".to_owned(), space_key(Some("p"))].into();
    let open = snapshot(&inputs, &spaces(), &toggled, now());
    assert!(open.find("mk0").is_some() && open.find("fold:mc").is_none());
    assert!(open.find("p0").is_some() && open.find("fold:space:p").is_none());
    assert!(open.find("fold:cc").is_some());
}

#[test]
fn eight_chats_never_fold() {
    let mut inputs = vec![input("c", "m", RunState::Working, 30)];
    for n in 0..7 {
        let mut kid = input(&format!("k{n}"), "m", RunState::Working, n);
        kid.parent_id = Some("c".into());
        inputs.push(kid);
    }
    let snap = snapshot(&inputs, &spaces(), &Toggled::new(), now());
    assert_eq!(snap.entries().count(), 8);
}

#[test]
fn a_parent_cycle_still_lists_every_chat() {
    let mut a = input("a", "m", RunState::Working, 2);
    a.parent_id = Some("b".into());
    let mut b = input("b", "m", RunState::Working, 1);
    b.parent_id = Some("a".into());
    let snap = snapshot(&[a, b], &spaces(), &Toggled::new(), now());
    assert_eq!(snap.entries().count(), 2);
}

#[test]
fn unknown_space_lands_in_no_space() {
    let snap = snapshot(
        &[input("x", "gone", RunState::Working, 1)],
        &spaces(),
        &Toggled::new(),
        now(),
    );
    assert_eq!(snap.sections[0].label, "No Space");
}

#[test]
fn stepping_wraps_and_survives_a_vanished_selection() {
    let inputs = vec![
        input("one", "m", RunState::Working, 3),
        input("two", "m", RunState::Working, 2),
        input("three", "m", RunState::Working, 1),
    ];
    let snap = snapshot(&inputs, &spaces(), &Toggled::new(), now());
    assert_eq!(step(&snap, None, 1).as_deref(), Some("one"));
    assert_eq!(step(&snap, None, -1).as_deref(), Some("three"));
    assert_eq!(step(&snap, Some("three"), 1).as_deref(), Some("one"));
    assert_eq!(step(&snap, Some("one"), -1).as_deref(), Some("three"));
    assert_eq!(step(&snap, Some("gone"), 1).as_deref(), Some("one"));
    assert_eq!(reconcile(&snap, Some("two")).as_deref(), Some("two"));
    assert_eq!(reconcile(&snap, Some("gone")).as_deref(), Some("one"));
}

#[test]
fn elapsed_formats() {
    assert_eq!(format_elapsed(8), "8s");
    assert_eq!(format_elapsed(300), "5m");
    assert_eq!(format_elapsed(4320), "1h 12m");
    assert_eq!(format_elapsed(7200), "2h");
    assert_eq!(format_elapsed(90_000), "1d 1h");
}
