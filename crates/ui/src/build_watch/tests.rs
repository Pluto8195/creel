use super::*;

fn stamp(inode: u64, mtime_ms: i64, len: u64) -> BuildStamp {
    BuildStamp {
        inode,
        mtime_ms,
        len,
    }
}

#[test]
fn identical_stamps_are_not_pending() {
    let running = stamp(10, 1000, 5);
    assert!(!is_newer_installed(&running, Some(&running)));
}

#[test]
fn a_swapped_inode_or_mtime_or_length_is_pending() {
    let running = stamp(10, 1000, 5);
    assert!(is_newer_installed(&running, Some(&stamp(11, 1000, 5))));
    assert!(is_newer_installed(&running, Some(&stamp(10, 2000, 5))));
    assert!(is_newer_installed(&running, Some(&stamp(10, 1000, 6))));
}

#[test]
fn an_unreadable_installed_binary_is_not_pending() {
    assert!(!is_newer_installed(&stamp(1, 1, 1), None));
}

#[test]
fn stamps_read_from_disk_change_when_the_file_is_replaced_like_the_installer_does() {
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("zeron");
    std::fs::write(&exe, b"old build").unwrap();
    let running = BuildStamp::of(&exe).unwrap();
    assert!(!is_newer_installed(&running, BuildStamp::of(&exe).as_ref()));
    // cp to a temp name, then mv -f over the executable.
    let staging = dir.path().join(".zeron.installing");
    std::fs::write(&staging, b"a newer build").unwrap();
    std::fs::rename(&staging, &exe).unwrap();
    assert!(is_newer_installed(&running, BuildStamp::of(&exe).as_ref()));
    assert_eq!(BuildStamp::of(&dir.path().join("missing")), None);
}

#[test]
fn marker_parses_sha_and_git_dir() {
    let id = parse_marker("0123abc:/Users/m/proj/.git\0junk").unwrap();
    assert_eq!(id.sha, "0123abc");
    assert_eq!(id.git_dir, "/Users/m/proj/.git");
    assert_eq!(parse_marker("nothex!:/x"), None);
    assert_eq!(parse_marker("abc:/x"), None);
    assert_eq!(parse_marker("no-colon"), None);
}

fn marked(prefix_len: usize, suffix_len: usize) -> Vec<u8> {
    let mut bytes = vec![7u8; prefix_len];
    bytes.extend_from_slice(marker_prefix().as_bytes());
    bytes.extend_from_slice(b"deadbeef12:/repo/.git\0");
    bytes.extend(std::iter::repeat_n(9u8, suffix_len));
    bytes
}

#[test]
fn marker_is_found_anywhere_including_across_chunk_edges() {
    for (prefix, suffix) in [(0, 0), (5, 3), (4090, 10), (4096 - 5, 2000), (100_000, 5000)] {
        let bytes = marked(prefix, suffix);
        for chunk in [64, 4096, 1 << 20] {
            let found = read_marker_from(bytes.as_slice(), chunk);
            assert_eq!(
                found,
                Some(BuildId {
                    sha: "deadbeef12".into(),
                    git_dir: "/repo/.git".into()
                }),
                "prefix {prefix} suffix {suffix} chunk {chunk}"
            );
        }
    }
}

#[test]
fn a_binary_without_a_marker_reads_none() {
    assert_eq!(read_marker_from(vec![1u8; 50_000].as_slice(), 4096), None);
    assert_eq!(read_marker_from(&b""[..], 4096), None);
}

#[test]
fn changes_use_the_branch_for_merges_and_the_subject_otherwise() {
    let log = "Merge running-chats-popover: popover and restart\n\
               Merge branch 'perf-round-3' into work-outline-implementation\n\
               \n\
               Point docs at the handoff\n";
    let changes = parse_changes(log);
    let labels: Vec<_> = changes.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "running-chats-popover",
            "perf-round-3",
            "Point docs at the handoff"
        ]
    );
}

#[test]
fn when_idle_waits_for_two_quiet_ticks_and_restarts_on_the_second() {
    let mut plan = RestartPlan::default();
    assert_eq!(plan.tick(0), RestartAction::None, "not armed");
    plan.arm();
    assert!(plan.is_armed());
    assert_eq!(plan.tick(2), RestartAction::None);
    assert_eq!(plan.tick(0), RestartAction::None);
    // A queued turn starting resets the quiet count.
    assert_eq!(plan.tick(1), RestartAction::None);
    assert_eq!(plan.tick(0), RestartAction::None);
    assert_eq!(plan.tick(0), RestartAction::Relaunch);
    assert_eq!(plan.state, RestartState::Relaunching);
    assert_eq!(plan.tick(0), RestartAction::None, "fires once");
}

#[test]
fn restart_now_is_immediate_when_nothing_is_mid_turn() {
    let mut plan = RestartPlan::default();
    assert_eq!(plan.request_now(0), RestartAction::Relaunch);
    assert_eq!(plan.request_now(0), RestartAction::None, "only once");
}

#[test]
fn restart_now_with_chats_mid_turn_waits_for_confirmation() {
    let mut plan = RestartPlan::default();
    assert_eq!(plan.request_now(3), RestartAction::None);
    assert_eq!(plan.state, RestartState::Confirming);
    assert_eq!(plan.tick(0), RestartAction::None, "the sheet is not armed");
    assert_eq!(plan.confirm(), RestartAction::Relaunch);
}

#[test]
fn waiting_on_the_sheet_cancels_and_a_stray_confirm_does_nothing() {
    let mut plan = RestartPlan::default();
    assert_eq!(plan.confirm(), RestartAction::None);
    plan.request_now(1);
    plan.cancel();
    assert_eq!(plan.state, RestartState::Idle);
    assert_eq!(plan.confirm(), RestartAction::None);
}

#[test]
fn the_sheet_can_hand_over_to_when_idle() {
    let mut plan = RestartPlan::default();
    plan.request_now(2);
    plan.arm();
    assert!(plan.is_armed());
    assert_eq!(plan.tick(0), RestartAction::None);
    assert_eq!(plan.tick(0), RestartAction::Relaunch);
}

#[test]
fn a_vanished_build_drops_the_plan_but_never_a_relaunch_in_flight() {
    let mut plan = RestartPlan::default();
    plan.arm();
    plan.build_gone();
    assert_eq!(plan.state, RestartState::Idle);
    plan.request_now(0);
    plan.build_gone();
    plan.cancel();
    assert_eq!(plan.state, RestartState::Relaunching);
}
