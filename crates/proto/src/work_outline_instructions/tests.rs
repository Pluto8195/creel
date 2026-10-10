use super::*;
use crate::work_outline::{WorkOutlineActorKind, WorkOutlineMutation, WorkOutlineQueryRequest};
use serde_json::json;

fn round_trip<T>(value: &T, expected: Value)
where
    T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    assert_eq!(serde_json::to_value(value).unwrap(), expected);
    assert_eq!(&serde_json::from_value::<T>(expected).unwrap(), value);
}

fn user() -> WorkOutlineActor {
    WorkOutlineActor {
        kind: WorkOutlineActorKind::User,
        device_id: "dev-1".into(),
        chat_id: None,
        message_id: None,
    }
}

fn item(id: &str, parent: Option<&str>, title: &str, inherit: bool) -> WorkItemView {
    WorkItemView {
        id: id.into(),
        space_id: "s-1".into(),
        parent_work_item_id: parent.map(Into::into),
        raw_parent_work_item_id: parent.map(Into::into),
        order_key: "a0".into(),
        title: title.into(),
        description: None,
        status: Default::default(),
        effective_status: Default::default(),
        archived_at: None,
        archived_by: None,
        archive_cause: None,
        inherit_parent_instructions: inherit,
        created_by: None,
        created_at: 0,
        updated_at: 0,
        child_ids: Vec::new(),
        depth: 0,
        chat_ids: Vec::new(),
        external_reference_ids: Vec::new(),
        rollup: Default::default(),
    }
}

fn instruction(
    id: &str,
    scope: Option<&str>,
    order_key: &str,
    text: &str,
) -> WorkItemInstructionView {
    WorkItemInstructionView {
        id: id.into(),
        space_id: "s-1".into(),
        work_item_id: scope.map(Into::into),
        text: text.into(),
        order_key: order_key.into(),
        created_by: Some(user()),
        created_at: 1,
        updated_at: 1,
        source: None,
        source_proposal_id: None,
        archived_at: None,
        archived_by: None,
    }
}

/// Space s-1: Platform (root) > Auth > Login, plus instructions on every
/// scope, one archived, one in another Space.
fn fixture(
    mid_inherits: bool,
    leaf_inherits: bool,
) -> (Vec<WorkItemView>, Vec<WorkItemInstructionView>) {
    let items = vec![
        item("w-root", None, "Platform", true),
        item("w-mid", Some("w-root"), "Auth", mid_inherits),
        item("w-leaf", Some("w-mid"), "Login", leaf_inherits),
    ];
    let mut archived = instruction("i-m2", Some("w-mid"), "a2", "Archived rule.");
    archived.archived_at = Some(5);
    let mut other_space = instruction("i-x", None, "a0", "Other Space rule.");
    other_space.space_id = "s-2".into();
    let mut sourced = instruction(
        "i-l0",
        Some("w-leaf"),
        "a1",
        "Keep the login form accessible.",
    );
    sourced.source = Some(InstructionSource {
        chat_id: Some("chat-1".into()),
        message_id: Some("msg-1".into()),
        excerpt: Some("keep the login form accessible".into()),
        content_hash: Some("abc".into()),
    });
    let instructions = vec![
        instruction("i-s1", None, "a1", "Prefer small commits."),
        instruction("i-s0", None, "a0", "Run targeted tests first."),
        instruction(
            "i-r1",
            Some("w-root"),
            "a0",
            "Flagship is a Django service.",
        ),
        instruction("i-m1", Some("w-mid"), "a0", "Do not change the public API."),
        archived,
        // Same order key as i-l0: the id breaks the tie.
        instruction("i-l1", Some("w-leaf"), "a1", "Summarize risks last."),
        sourced,
        other_space,
    ];
    (items, instructions)
}

fn ids(effective: &EffectiveInstructions) -> Vec<&str> {
    effective
        .entries
        .iter()
        .map(|e| e.instruction_id.as_str())
        .collect()
}

// ── resolver ────────────────────────────────────────────────────────────────

#[test]
fn resolver_orders_space_root_then_ancestors_then_local() {
    let (items, instructions) = fixture(true, true);
    let effective = resolve_effective_instructions_in("w-leaf", &items, &instructions);
    assert_eq!(
        ids(&effective),
        ["i-s0", "i-s1", "i-r1", "i-m1", "i-l0", "i-l1"]
    );
    assert_eq!(effective.boundary_work_item_id, None);
    let orders: Vec<u32> = effective.entries.iter().map(|e| e.order).collect();
    assert_eq!(orders, [0, 1, 2, 3, 4, 5]);
    let inherited: Vec<bool> = effective.entries.iter().map(|e| e.inherited).collect();
    assert_eq!(inherited, [true, true, true, true, false, false]);
    let first = &effective.entries[0];
    assert_eq!(first.source_kind, InstructionSourceKind::SpaceRoot);
    assert_eq!(first.scope_id, "s-1");
    assert_eq!(first.scope_title, SPACE_ROOT_SCOPE_TITLE);
    let mid = &effective.entries[3];
    assert_eq!(mid.source_kind, InstructionSourceKind::WorkItem);
    assert_eq!(
        (mid.scope_id.as_str(), mid.scope_title.as_str()),
        ("w-mid", "Auth")
    );
    // Provenance keeps the source ids and hash, drops the excerpt.
    let sourced = &effective.entries[4];
    let source = sourced.provenance.source.as_ref().unwrap();
    assert_eq!(source.chat_id.as_deref(), Some("chat-1"));
    assert_eq!(source.content_hash.as_deref(), Some("abc"));
    assert!(source.excerpt.is_none());
    assert_eq!(sourced.provenance.created_by, Some(user()));
    assert_eq!(effective.entry_count, 6);
    let chars: usize = effective
        .entries
        .iter()
        .map(|e| e.text.chars().count())
        .sum();
    assert_eq!(effective.rendered_chars as usize, chars);
    assert!(effective.within_budget && effective.budget_error.is_none());
    assert_eq!(effective.precedence_rule, INSTRUCTION_PRECEDENCE_RULE);
    assert_eq!(
        effective.budget,
        InstructionBudget {
            max_chars: 9_000,
            max_entries: 32
        }
    );
}

#[test]
fn resolver_boundary_at_middle_drops_ancestors_above_it() {
    let (items, instructions) = fixture(false, true);
    let effective = resolve_effective_instructions_in("w-leaf", &items, &instructions);
    assert_eq!(ids(&effective), ["i-s0", "i-s1", "i-m1", "i-l0", "i-l1"]);
    assert_eq!(effective.boundary_work_item_id.as_deref(), Some("w-mid"));
    // The boundary item itself still sees its own instructions.
    let mid = resolve_effective_instructions_in("w-mid", &items, &instructions);
    assert_eq!(ids(&mid), ["i-s0", "i-s1", "i-m1"]);
    assert!(!mid.entries[2].inherited);
}

#[test]
fn resolver_boundary_at_self_keeps_space_root_and_local_only() {
    let (items, instructions) = fixture(true, false);
    let effective = resolve_effective_instructions_in("w-leaf", &items, &instructions);
    assert_eq!(ids(&effective), ["i-s0", "i-s1", "i-l0", "i-l1"]);
    assert_eq!(effective.boundary_work_item_id.as_deref(), Some("w-leaf"));
}

#[test]
fn resolver_always_applies_space_root_and_excludes_archived_and_other_spaces() {
    let (items, mut instructions) = fixture(false, false);
    let effective = resolve_effective_instructions_in("w-leaf", &items, &instructions);
    assert!(ids(&effective).starts_with(&["i-s0", "i-s1"]));
    assert!(!ids(&effective).contains(&"i-x"));
    let mid = resolve_effective_instructions_in("w-mid", &items, &instructions);
    assert!(!ids(&mid).contains(&"i-m2"), "archived is excluded");
    // Archiving a Space-root instruction removes it everywhere.
    instructions[0].archived_at = Some(9);
    let effective = resolve_effective_instructions_in("w-leaf", &items, &instructions);
    assert_eq!(ids(&effective), ["i-s0", "i-l0", "i-l1"]);
}

#[test]
fn resolver_handles_unknown_items_and_input_order() {
    let (items, mut instructions) = fixture(true, true);
    let missing = resolve_effective_instructions_in("w-missing", &items, &instructions);
    assert!(missing.entries.is_empty() && missing.within_budget);
    instructions.reverse();
    let mut reversed_items = items.clone();
    reversed_items.reverse();
    assert_eq!(
        resolve_effective_instructions_in("w-leaf", &reversed_items, &instructions),
        resolve_effective_instructions_in("w-leaf", &items, &{
            let mut i = instructions.clone();
            i.reverse();
            i
        })
    );
}

#[test]
fn resolver_reports_over_budget_without_failing() {
    let (items, mut instructions) = fixture(true, true);
    for n in 0..40 {
        instructions.push(instruction(
            &format!("i-z{n:02}"),
            Some("w-leaf"),
            "b0",
            "x",
        ));
    }
    let effective = resolve_effective_instructions_in("w-leaf", &items, &instructions);
    assert_eq!(effective.entry_count, 46);
    assert!(!effective.within_budget);
    assert!(effective.budget_error.unwrap().contains("at most 32"));
}

#[test]
fn snapshot_effective_instructions_uses_its_items_and_instructions() {
    let (items, instructions) = fixture(true, true);
    let snapshot = crate::WorkOutlineSnapshot {
        space_id: "s-1".into(),
        items: items.clone(),
        instructions: instructions.clone(),
        ..Default::default()
    };
    assert_eq!(
        snapshot.effective_instructions("w-leaf"),
        resolve_effective_instructions_in("w-leaf", &items, &instructions)
    );
}

// ── revisions: render, hash, budgets, integrity ─────────────────────────────

fn fixture_entries() -> Vec<InstructionRevisionEntry> {
    let (items, instructions) = fixture(true, true);
    resolve_effective_instructions_in("w-leaf", &items, &instructions).revision_entries()
}
