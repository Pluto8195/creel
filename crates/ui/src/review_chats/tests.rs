//! The review-chat rule: which chats count, how they sort, and the strings
//! the three surfaces share. Pure: snapshots and facts are filled by hand.

use chrono::{DateTime, Utc};
use zeron_proto::{ChatIndicator, ChatLinkSource, PullRequestRef};

use super::*;
use crate::github::fixture as gf;
use crate::work_outline::store::{fixture_ids::*, fixture_snapshot};

fn at(secs: i64) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp(secs, 0)
}

fn fact(id: &str, title: &str, secs: i64) -> ChatFact {
    ChatFact {
        id: id.into(),
        title: title.into(),
        archived: false,
        updated: at(secs),
        state: ChatIndicator::Idle,
    }
}

fn link(id: &str, source: ChatLinkSource) -> (String, ChatLinkSource) {
    (id.into(), source)
}

fn ids(chats: &[ReviewChat]) -> Vec<&str> {
    chats.iter().map(|chat| chat.chat_id.as_str()).collect()
}

#[test]
fn a_chat_assigned_to_the_item_is_a_review_chat() {
    let chats = resolve(
        &["a".into()],
        &[],
        &[fact("a", "Review it", 5), fact("b", "Other", 9)],
    );
    assert_eq!(ids(&chats), ["a"]);
    assert_eq!(chats[0].reason, ReviewReason::Assigned);
    assert_eq!(chats[0].title, "Review it");
}

#[test]
fn manual_and_launcher_links_count() {
    let facts = [fact("m", "Manual", 1), fact("l", "Launched", 2)];
    let links = [
        link("m", ChatLinkSource::Manual),
        link("l", ChatLinkSource::ReviewLauncher),
    ];
    let chats = resolve(&[], &links, &facts);
    assert_eq!(ids(&chats), ["l", "m"]);
    assert_eq!(
        chats[0].reason,
        ReviewReason::Linked(ChatLinkSource::ReviewLauncher)
    );
    assert_eq!(
        chats[1].reason,
        ReviewReason::Linked(ChatLinkSource::Manual)
    );
}

#[test]
fn mention_branch_ticket_diff_target_and_created_in_chat_links_do_not_count() {
    let sources = [
        ChatLinkSource::Mentioned,
        ChatLinkSource::Branch,
        ChatLinkSource::Ticket,
        ChatLinkSource::DiffTarget,
        ChatLinkSource::CreatedInChat,
    ];
    let facts: Vec<ChatFact> = (0..sources.len())
        .map(|i| fact(&format!("c{i}"), "c", 1))
        .collect();
    let links: Vec<_> = sources
        .iter()
        .enumerate()
        .map(|(i, source)| link(&format!("c{i}"), *source))
        .collect();
    assert!(resolve(&[], &links, &facts).is_empty());
}

#[test]
fn archived_and_unknown_chats_are_excluded() {
    let mut archived = fact("old", "Old review", 1);
    archived.archived = true;
    let facts = [archived, fact("live", "Live review", 2)];
    let chats = resolve(
        &["old".into(), "gone".into()],
        &[
            link("live", ChatLinkSource::Manual),
            link("gone", ChatLinkSource::Manual),
        ],
        &facts,
    );
    assert_eq!(ids(&chats), ["live"]);
}

#[test]
fn several_review_chats_sort_most_recent_first_and_assignment_wins_the_reason() {
    let facts = [
        fact("old", "Old", 10),
        fact("new", "New", 30),
        fact("mid", "Mid", 20),
    ];
    let chats = resolve(
        &["mid".into()],
        &[
            link("old", ChatLinkSource::Manual),
            link("new", ChatLinkSource::ReviewLauncher),
            link("mid", ChatLinkSource::Manual),
        ],
        &facts,
    );
    assert_eq!(ids(&chats), ["new", "mid", "old"]);
    assert_eq!(chats[1].reason, ReviewReason::Assigned);
    assert_eq!(primary(&chats).map(|c| c.chat_id.as_str()), Some("new"));
    assert_eq!(ids(others(&chats)), ["mid", "old"]);
}

#[test]
fn a_finished_or_idle_review_chat_still_counts() {
    let mut finished = fact("f", "Done", 1);
    finished.state = ChatIndicator::Completed;
    let chats = resolve(
        &["f".into(), "i".into()],
        &[],
        &[finished, fact("i", "Idle", 2)],
    );
    assert_eq!(ids(&chats), ["i", "f"]);
}

#[test]
fn an_untitled_chat_gets_a_title_and_ties_sort_by_id() {
    let chats = resolve(
        &["b".into(), "a".into()],
        &[],
        &[fact("b", "  ", 5), fact("a", "Named", 5)],
    );
    assert_eq!(ids(&chats), ["a", "b"]);
    assert_eq!(chats[1].title, "Untitled chat");
}

#[test]
fn enter_opens_the_most_recent_review_chat_or_starts_a_review() {
    assert_eq!(enter_action(&[]), EnterAction::StartReview);
    let chats = resolve(
        &["a".into(), "b".into()],
        &[],
        &[fact("a", "A", 1), fact("b", "B", 2)],
    );
    assert_eq!(enter_action(&chats), EnterAction::OpenReview("b".into()));
}

#[test]
fn state_labels_and_the_marker_text() {
    assert_eq!(state_label(ChatIndicator::Working), "working");
    assert_eq!(state_label(ChatIndicator::AwaitingInput), "awaiting input");
    assert_eq!(state_label(ChatIndicator::Completed), "finished");
    assert_eq!(state_label(ChatIndicator::Idle), "idle");
    assert_eq!(marker_label(&[]), None);

    let mut working = fact("a", "Review flagship#9", 2);
    working.state = ChatIndicator::Working;
    let chats = resolve(
        &["a".into(), "b".into()],
        &[],
        &[working, fact("b", "Earlier", 1)],
    );
    assert_eq!(
        marker_label(&chats).as_deref(),
        Some("Being reviewed · Review flagship#9 · working +1")
    );
    assert_eq!(open_review_detail(&chats[0]), "Review flagship#9 · working");
}

#[test]
fn assignment_comes_from_review_items_that_link_the_pr() {
    let mut snapshot = fixture_snapshot("s1");
    snapshot.space_settings.pr_review_sync_work_item_id = Some(LAUNCH.into());
    snapshot.external_references = vec![gf::reference("r1", "s1", API, "app", 7)];
    let pr = PullRequestRef::new("acme", "app", 7);
    let assigned = assigned_chat_ids(&snapshot, &pr);
    assert!(assigned.contains(&CHAT_API.to_owned()));
    // Another PR, and an item that does not link this one, add nothing.
    assert!(assigned_chat_ids(&snapshot, &PullRequestRef::new("acme", "app", 8)).is_empty());
    assert!(!assigned.contains(&CHAT_UI.to_owned()));
}

#[test]
fn a_feature_item_that_merely_links_a_pr_is_not_a_review_item() {
    let mut snapshot = fixture_snapshot("s1");
    // No sync destination, not a synced item, title does not start with the
    // PR: `w-api` is an ordinary item that links its own PR.
    snapshot.external_references = vec![gf::reference("r1", "s1", API, "app", 7)];
    let pr = PullRequestRef::new("acme", "app", 7);
    assert!(assigned_chat_ids(&snapshot, &pr).is_empty());
}

#[test]
fn an_archived_review_item_still_assigns_its_chats() {
    let mut snapshot = fixture_snapshot("s1");
    snapshot.space_settings.pr_review_sync_work_item_id = Some(LAUNCH.into());
    snapshot.external_references = vec![gf::reference("r1", "s1", API, "app", 7)];
    for item in &mut snapshot.items {
        if item.id == API {
            item.archived_at = Some(1);
        }
    }
    let pr = PullRequestRef::new("acme", "app", 7);
    assert!(assigned_chat_ids(&snapshot, &pr).contains(&CHAT_API.to_owned()));
}

#[test]
fn the_index_keeps_the_strongest_source_and_replaces_per_pr_and_per_chat() {
    let pr = PullRequestRef::new("acme", "app", 7);
    let url = "https://github.com/acme/app/pull/7";
    let mut index = PrChatLinkIndex::default();
    index.set_pr(
        url,
        &[
            link("a", ChatLinkSource::Branch),
            link("a", ChatLinkSource::Manual),
            link("b", ChatLinkSource::Mentioned),
        ],
    );
    let mut links = index.links(&pr);
    links.sort_by(|x, y| x.0.cmp(&y.0));
    assert_eq!(
        links,
        [
            link("a", ChatLinkSource::Manual),
            link("b", ChatLinkSource::Mentioned)
        ]
    );

    // A chat's fresh status replaces what was known about that chat only.
    index.set_chat("a", &[]);
    assert_eq!(index.links(&pr), [link("b", ChatLinkSource::Mentioned)]);
    index.set_chat("c", &[(url.to_owned(), ChatLinkSource::ReviewLauncher)]);
    assert_eq!(index.links(&pr).len(), 2);
    // Case and trailing path do not matter.
    index.set_pr("https://github.com/Acme/App/pull/7/files", &[]);
    assert!(index.links(&pr).is_empty());
}
