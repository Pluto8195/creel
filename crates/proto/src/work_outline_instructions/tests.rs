use super::*;
use crate::work_outline::{WorkOutlineActorKind, WorkOutlineQueryRequest};
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
        external_sync_modified_fields: Vec::new(),
        external_sync_state: None,
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
    // A work-item instruction stamped with another Space is an orphan.
    let mut cross = instruction("i-cross", Some("w-leaf"), "a0", "Cross-Space rule.");
    cross.space_id = "s-2".into();
    instructions.push(cross);
    let effective = resolve_effective_instructions_in("w-leaf", &items, &instructions);
    assert!(!ids(&effective).contains(&"i-cross"));
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

fn entry(
    id: &str,
    kind: InstructionSourceKind,
    scope_id: &str,
    scope_title: &str,
    text: &str,
) -> InstructionRevisionEntry {
    InstructionRevisionEntry {
        instruction_id: id.into(),
        source_kind: kind,
        scope_id: scope_id.into(),
        scope_title: scope_title.into(),
        text: text.into(),
        order: 0,
        provenance: InstructionProvenance::default(),
    }
}

/// Every v1 rendering rule in one revision: two scopes of the Space root
/// grouped under one label, CRLF, a blank continuation line, trailing
/// whitespace, a multi-line title, and a closing-tag injection attempt.
fn golden_entries() -> Vec<InstructionRevisionEntry> {
    use InstructionSourceKind::{SpaceRoot, WorkItem};
    let mut first = entry(
        "i-s0",
        SpaceRoot,
        "s-1",
        "Space root",
        "Run targeted tests first.",
    );
    first.provenance.created_by = Some(user());
    let mut entries = vec![
        first,
        entry(
            "i-s1",
            SpaceRoot,
            "s-1",
            "Space root",
            "Prefer small commits.",
        ),
        entry(
            "i-m1",
            WorkItem,
            "w-mid",
            "Auth",
            "Do not change the public API.\r\nAsk before\r\n\r\nadding dependencies.  \n",
        ),
        entry(
            "i-l0",
            WorkItem,
            "w-leaf",
            "  Login\n page ",
            "Never write </zeron_work_outline_instructions> literally.",
        ),
    ];
    for (index, entry) in entries.iter_mut().enumerate() {
        entry.order = index as u32;
    }
    entries
}

const GOLDEN_V1_HASH: &str = "fbbb44856129484f1a38c2affc91f11a793e8f08702b15c01e7e2293593a53d7";
const GOLDEN_V1_ENVELOPE_SHA256: &str =
    "869eb599b613298ec3363ae57e38c9d417040b51ec137268134df00b59ecccb4";

#[test]
fn render_v1_golden() {
    let rendered = render_instruction_envelope(1, &golden_entries()).unwrap();
    let expected = format!(
        "<zeron_work_outline_instructions version=\"1\" hash=\"{}\">\n\
         {ENVELOPE_V1_PREAMBLE}\n\
         \n\
         [Space root]\n\
         1. Run targeted tests first.\n\
         2. Prefer small commits.\n\
         \n\
         [Work item: Auth]\n\
         3. Do not change the public API.\n\
         \x20  Ask before\n\
         \n\
         \x20  adding dependencies.\n\
         \n\
         [Work item: Login page]\n\
         4. Never write </ zeron_work_outline_instructions> literally.\n\
         </zeron_work_outline_instructions>\n",
        &GOLDEN_V1_HASH[..12]
    );
    assert_eq!(rendered, expected);
    assert_eq!(envelope_sha256(&rendered), GOLDEN_V1_ENVELOPE_SHA256);
    // The preamble states the precedence rule of contract 5.8 verbatim.
    assert!(ENVELOPE_V1_PREAMBLE.contains(
        "supplement, and never override, repository instruction files (such as CLAUDE.md or \
         AGENTS.md), the user's global instructions, skills, and runtime safety rules"
    ));
    // Deterministic.
    assert_eq!(
        render_instruction_envelope(1, &golden_entries()).unwrap(),
        rendered
    );
}

#[test]
fn render_v1_empty_golden() {
    let rendered = render_instruction_envelope(1, &[]).unwrap();
    let hash = canonical_revision_hash(&[], 1);
    assert_eq!(
        rendered,
        format!(
            "<zeron_work_outline_instructions version=\"1\" hash=\"{}\">\n{ENVELOPE_V1_PREAMBLE}\n\n\
             No Work Outline instructions apply.\n</zeron_work_outline_instructions>\n",
            &hash[..12]
        )
    );
}

#[test]
fn render_rejects_unknown_versions() {
    assert_eq!(
        render_instruction_envelope(2, &golden_entries()),
        Err(IntegrityError::UnsupportedEnvelopeVersion(2))
    );
    assert_eq!(
        render_instruction_envelope(0, &[]),
        Err(IntegrityError::UnsupportedEnvelopeVersion(0))
    );
}

#[test]
fn canonical_json_sorts_keys_without_whitespace() {
    let value = json!({"b": 1, "a": [1, {"d": "x\n\"", "c": null}], "é": true});
    assert_eq!(
        canonical_json(&value),
        r#"{"a":[1,{"c":null,"d":"x\n\""}],"b":1,"é":true}"#
    );
}

#[test]
fn canonical_revision_hash_golden_and_stability() {
    let entries = golden_entries();
    assert_eq!(canonical_revision_hash(&entries, 1), GOLDEN_V1_HASH);
    // The canonical form of one entry: sorted keys, absent options omitted.
    assert_eq!(
        canonical_json(&serde_json::to_value(&entries[1]).unwrap()),
        r#"{"instructionId":"i-s1","order":1,"provenance":{},"scopeId":"s-1","scopeTitle":"Space root","sourceKind":"spaceRoot","text":"Prefer small commits."}"#
    );
    // Field order on the wire does not matter: decode from shuffled keys.
    let shuffled: Vec<InstructionRevisionEntry> = entries
        .iter()
        .map(|e| {
            let Value::Object(map) = serde_json::to_value(e).unwrap() else {
                unreachable!()
            };
            let reversed: serde_json::Map<String, Value> = map.into_iter().rev().collect();
            serde_json::from_value(Value::Object(reversed)).unwrap()
        })
        .collect();
    assert_eq!(canonical_revision_hash(&shuffled, 1), GOLDEN_V1_HASH);
    // Every hashed input matters.
    let mut text = entries.clone();
    text[0].text.push('!');
    assert_ne!(canonical_revision_hash(&text, 1), GOLDEN_V1_HASH);
    let mut title = entries.clone();
    title[2].scope_title = "Auth v2".into();
    assert_ne!(canonical_revision_hash(&title, 1), GOLDEN_V1_HASH);
    assert_ne!(canonical_revision_hash(&entries, 2), GOLDEN_V1_HASH);
}

// ── budgets ─────────────────────────────────────────────────────────────────

#[test]
fn build_revision_body_renumbers_and_verifies() {
    let mut entries = golden_entries();
    for entry in &mut entries {
        entry.order = 99;
    }
    let body = build_revision_body(entries).unwrap();
    assert_eq!(body.entries, golden_entries());
    assert_eq!(body.envelope_version, INSTRUCTION_ENVELOPE_VERSION);
    assert_eq!(body.hash, GOLDEN_V1_HASH);
    assert_eq!(body.envelope_sha256, GOLDEN_V1_ENVELOPE_SHA256);
    assert_eq!(verify_revision_body(&body), Ok(()));
    // Empty is a valid revision.
    let empty = build_revision_body(Vec::new()).unwrap();
    assert_eq!(verify_revision_body(&empty), Ok(()));
    // The resolver's entries build too.
    assert!(build_revision_body(fixture_entries()).is_ok());
}

fn texts(count: usize, chars: usize) -> Vec<InstructionRevisionEntry> {
    (0..count)
        .map(|n| {
            entry(
                &format!("i-{n:03}"),
                InstructionSourceKind::WorkItem,
                "w",
                "W",
                &"x".repeat(chars),
            )
        })
        .collect()
}

fn limit_error(entries: Vec<InstructionRevisionEntry>) -> WorkOutlineError {
    let error = build_revision_body(entries).unwrap_err();
    assert_eq!(error.code, WorkOutlineErrorCode::LimitExceeded);
    error
}

#[test]
fn build_revision_body_enforces_budgets() {
    assert_eq!(limits::MAX_EFFECTIVE_INSTRUCTION_CHARS, 9_000);
    assert_eq!(limits::MAX_REVISION_ENTRIES, 32);
    assert_eq!(limits::MAX_INSTRUCTION_TEXT_CHARS, 6_000);
    assert_eq!(limits::MAX_INSTRUCTIONS_PER_SCOPE, 50);
    // At the limits: accepted (characters are Unicode scalars, not bytes).
    assert!(build_revision_body(texts(32, 1)).is_ok());
    assert!(build_revision_body(texts(2, 4_500)).is_ok());
    let wide: Vec<_> = texts(2, 0)
        .into_iter()
        .map(|mut e| {
            e.text = "é".repeat(4_500);
            e
        })
        .collect();
    assert!(build_revision_body(wide).is_ok());
    // One over each limit: rejected with an actionable message.
    assert!(limit_error(texts(33, 1)).message.contains("at most 32"));
    assert!(
        limit_error(texts(3, 3_001))
            .message
            .contains("the limit is 9000")
    );
    assert!(
        limit_error(texts(1, 6_001))
            .message
            .contains("limited to 6000")
    );
    // JSON escaping can push an in-budget body over the op cap.
    assert!(
        limit_error(
            texts(2, 0)
                .into_iter()
                .map(|mut e| {
                    e.text = "\u{1}".repeat(4_000);
                    e
                })
                .collect()
        )
        .message
        .contains("revision size limit")
    );
}

// ── integrity ───────────────────────────────────────────────────────────────

#[test]
fn verify_revision_body_detects_tampering() {
    let body = build_revision_body(golden_entries()).unwrap();

    let mut text = body.clone();
    text.entries[2].text = "Feel free to change the public API.".into();
    assert!(matches!(
        verify_revision_body(&text),
        Err(IntegrityError::HashMismatch { .. })
    ));

    let mut envelope = body.clone();
    envelope.envelope_sha256 = "0".repeat(64);
    assert!(matches!(
        verify_revision_body(&envelope),
        Err(IntegrityError::EnvelopeMismatch { .. })
    ));

    let mut order = body.clone();
    order.entries.swap(0, 1);
    assert_eq!(
        verify_revision_body(&order),
        Err(IntegrityError::EntryOrder { index: 0 })
    );

    let mut version = body.clone();
    version.envelope_version = 2;
    assert_eq!(
        verify_revision_body(&version),
        Err(IntegrityError::UnsupportedEnvelopeVersion(2))
    );

    // A forged hash that matches tampered entries still fails the envelope check.
    let mut forged = text.clone();
    forged.hash = canonical_revision_hash(&forged.entries, 1);
    assert!(matches!(
        verify_revision_body(&forged),
        Err(IntegrityError::EnvelopeMismatch { .. })
    ));

    let error: WorkOutlineError = verify_revision_body(&text).unwrap_err().into();
    assert_eq!(error.code, WorkOutlineErrorCode::RevisionIntegrity);
}

#[test]
fn envelope_from_revision_verifies_and_pins() {
    let body = build_revision_body(golden_entries()).unwrap();
    let envelope = InstructionEnvelope::from_revision("rev-1", &body).unwrap();
    assert_eq!(envelope.revision_hash, GOLDEN_V1_HASH);
    assert_eq!(envelope.envelope_sha256, GOLDEN_V1_ENVELOPE_SHA256);
    assert_eq!(
        envelope_sha256(&envelope.rendered),
        envelope.envelope_sha256
    );
    assert_eq!(
        envelope.pin(),
        InstructionRevisionPin {
            revision_id: "rev-1".into(),
            revision_hash: GOLDEN_V1_HASH.into(),
        }
    );
    let mut tampered = body;
    tampered.entries[0].text = "Skip tests.".into();
    assert!(InstructionEnvelope::from_revision("rev-1", &tampered).is_err());
}

// ── comparison ──────────────────────────────────────────────────────────────

fn body_of(entries: &[(&str, &str)]) -> InstructionRevisionBody {
    build_revision_body(
        entries
            .iter()
            .map(|(id, text)| entry(id, InstructionSourceKind::WorkItem, "w", "W", text))
            .collect(),
    )
    .unwrap()
}

#[test]
fn compare_reports_added_removed_changed_and_reordered() {
    let source = body_of(&[("a", "A"), ("b", "B"), ("c", "C"), ("d", "D")]);
    let target = body_of(&[("c", "C"), ("a", "A"), ("b", "B2"), ("e", "E")]);
    let comparison = compare_instruction_revisions(&source, &target).unwrap();
    assert!(!comparison.identical);
    assert_eq!(comparison.source_hash, source.hash);
    assert_eq!(comparison.target_hash, target.hash);
    let ids = |entries: &[InstructionRevisionEntry]| {
        entries
            .iter()
            .map(|e| e.instruction_id.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&comparison.added), ["e"]);
    assert_eq!(ids(&comparison.removed), ["d"]);
    assert_eq!(
        comparison.changed,
        [InstructionTextChange {
            instruction_id: "b".into(),
            before_text: "B".into(),
            after_text: "B2".into(),
        }]
    );
    // LCS of [a b c] vs [c a b] keeps a, b: c moved.
    assert_eq!(comparison.reordered, ["c"]);
    let changed_lines: Vec<(TextDiffKind, &str)> = comparison
        .line_diff
        .iter()
        .filter(|l| l.kind != TextDiffKind::Same)
        .map(|l| (l.kind, l.text.as_str()))
        .collect();
    assert_eq!(
        changed_lines,
        [
            (
                TextDiffKind::Removed,
                &*format!(
                    "<zeron_work_outline_instructions version=\"1\" hash=\"{}\">",
                    &source.hash[..12]
                )
            ),
            (
                TextDiffKind::Added,
                &*format!(
                    "<zeron_work_outline_instructions version=\"1\" hash=\"{}\">",
                    &target.hash[..12]
                )
            ),
            (TextDiffKind::Removed, "1. A"),
            (TextDiffKind::Removed, "2. B"),
            (TextDiffKind::Removed, "3. C"),
            (TextDiffKind::Removed, "4. D"),
            (TextDiffKind::Added, "1. C"),
            (TextDiffKind::Added, "2. A"),
            (TextDiffKind::Added, "3. B2"),
            (TextDiffKind::Added, "4. E"),
        ]
    );
    // Deterministic.
    assert_eq!(
        compare_instruction_revisions(&source, &target).unwrap(),
        comparison
    );
}

#[test]
fn compare_identical_revisions_is_empty() {
    let body = build_revision_body(golden_entries()).unwrap();
    let comparison = compare_instruction_revisions(&body, &body).unwrap();
    assert!(comparison.identical);
    assert!(comparison.added.is_empty() && comparison.removed.is_empty());
    assert!(comparison.changed.is_empty() && comparison.reordered.is_empty());
    assert!(
        comparison
            .line_diff
            .iter()
            .all(|l| l.kind == TextDiffKind::Same)
    );
    let mut bad = body.clone();
    bad.envelope_version = 9;
    assert!(compare_instruction_revisions(&body, &bad).is_err());
}

#[test]
fn diff_lines_is_a_minimal_lcs_diff() {
    let diff = diff_lines("a\nb\nc\nd\n", "a\nc\nx\nd\n");
    let pairs: Vec<(TextDiffKind, &str)> = diff.iter().map(|l| (l.kind, l.text.as_str())).collect();
    assert_eq!(
        pairs,
        [
            (TextDiffKind::Same, "a"),
            (TextDiffKind::Removed, "b"),
            (TextDiffKind::Same, "c"),
            (TextDiffKind::Added, "x"),
            (TextDiffKind::Same, "d"),
        ]
    );
    assert!(diff_lines("", "").is_empty());
    assert_eq!(
        diff_lines("", "z"),
        [TextDiffLine {
            kind: TextDiffKind::Added,
            text: "z".into()
        }]
    );
}

// ── wire field names ────────────────────────────────────────────────────────

const USER_JSON: &str = r#"{"kind":"user","deviceId":"dev-1"}"#;

fn user_json() -> Value {
    serde_json::from_str(USER_JSON).unwrap()
}

#[test]
fn instruction_wire_names_are_pinned() {
    round_trip(&InstructionScope::SpaceRoot, json!({"type":"spaceRoot"}));
    round_trip(
        &InstructionScope::WorkItem {
            work_item_id: "w".into(),
        },
        json!({"type":"workItem","workItemId":"w"}),
    );
    let source = InstructionSource {
        chat_id: Some("c".into()),
        message_id: Some("m".into()),
        excerpt: Some("x".into()),
        content_hash: Some("h".into()),
    };
    round_trip(
        &source,
        json!({"chatId":"c","messageId":"m","excerpt":"x","contentHash":"h"}),
    );
    round_trip(&InstructionSource::default(), json!({}));
    let view = WorkItemInstructionView {
        id: "i".into(),
        space_id: "s".into(),
        work_item_id: Some("w".into()),
        text: "T".into(),
        order_key: "a0".into(),
        created_by: Some(user()),
        created_at: 1,
        updated_at: 2,
        source: Some(source.clone()),
        source_proposal_id: Some("p".into()),
        archived_at: Some(3),
        archived_by: Some(user()),
    };
    round_trip(
        &view,
        json!({
            "id":"i","spaceId":"s","workItemId":"w","text":"T","orderKey":"a0",
            "createdBy":user_json(),"createdAt":1,"updatedAt":2,
            "source":{"chatId":"c","messageId":"m","excerpt":"x","contentHash":"h"},
            "sourceProposalId":"p","archivedAt":3,"archivedBy":user_json()
        }),
    );
    // Space-root instruction: `workItemId` absent; lenient decode.
    let root: WorkItemInstructionView = serde_json::from_value(json!({"id":"i"})).unwrap();
    assert_eq!(root.scope(), InstructionScope::SpaceRoot);
    assert!(!root.archived());
    // Provenance drops the excerpt.
    assert_eq!(
        serde_json::to_value(view.provenance()).unwrap(),
        json!({"createdBy":user_json(),"source":{"chatId":"c","messageId":"m","contentHash":"h"},
               "sourceProposalId":"p"})
    );
}

#[test]
fn effective_instruction_wire_names_are_pinned() {
    let entry = EffectiveInstructionEntry {
        instruction_id: "i".into(),
        source_kind: InstructionSourceKind::SpaceRoot,
        scope_id: "s".into(),
        scope_title: "Space root".into(),
        text: "T".into(),
        order: 0,
        inherited: true,
        provenance: InstructionProvenance::default(),
    };
    let entry_json = json!({
        "instructionId":"i","sourceKind":"spaceRoot","scopeId":"s","scopeTitle":"Space root",
        "text":"T","order":0,"inherited":true,"provenance":{}
    });
    round_trip(&entry, entry_json.clone());
    round_trip(
        &EffectiveInstructions {
            work_item_id: "w".into(),
            boundary_work_item_id: Some("b".into()),
            entries: vec![entry],
            precedence_rule: "rule".into(),
            rendered_chars: 1,
            entry_count: 1,
            within_budget: false,
            budget_error: Some("too big".into()),
            budget: InstructionBudget::default(),
        },
        json!({
            "workItemId":"w","boundaryWorkItemId":"b","entries":[entry_json],
            "precedenceRule":"rule","renderedChars":1,"entryCount":1,"withinBudget":false,
            "budgetError":"too big","budget":{"maxChars":9000,"maxEntries":32}
        }),
    );
    // Lenient source kind.
    assert_eq!(
        serde_json::from_value::<InstructionSourceKind>(json!("future")).unwrap(),
        InstructionSourceKind::WorkItem
    );
    round_trip(&InstructionSourceKind::WorkItem, json!("workItem"));
}

#[test]
fn revision_wire_names_are_pinned() {
    let body = InstructionRevisionBody {
        entries: vec![entry("i", InstructionSourceKind::WorkItem, "w", "W", "T")],
        envelope_version: 1,
        envelope_sha256: "e".into(),
        hash: "h".into(),
    };
    let body_json = json!({
        "entries":[{"instructionId":"i","sourceKind":"workItem","scopeId":"w","scopeTitle":"W",
                    "text":"T","order":0,"provenance":{}}],
        "envelopeVersion":1,"envelopeSha256":"e","hash":"h"
    });
    round_trip(&body, body_json.clone());
    let revision = InstructionRevisionView {
        id: "r".into(),
        space_id: "s".into(),
        work_item_id: "w".into(),
        body,
        created_by: Some(user()),
        created_at: 5,
        integrity: InstructionRevisionIntegrity::Ok,
    };
    round_trip(
        &revision,
        json!({"id":"r","spaceId":"s","workItemId":"w","body":body_json,
               "createdBy":user_json(),"createdAt":5,"integrity":"ok"}),
    );
    // Missing or unknown integrity fails closed.
    let missing: InstructionRevisionView =
        serde_json::from_value(json!({"id":"r","body":{}})).unwrap();
    assert_eq!(missing.integrity, InstructionRevisionIntegrity::Corrupt);
    round_trip(&InstructionRevisionIntegrity::Corrupt, json!("corrupt"));
    assert_eq!(
        serde_json::from_value::<InstructionRevisionIntegrity>(json!("weird")).unwrap(),
        InstructionRevisionIntegrity::Corrupt
    );

    let application = InstructionRevisionApplicationView {
        id: "a".into(),
        space_id: "s".into(),
        chat_id: "c".into(),
        work_item_id: "w".into(),
        revision_id: "r".into(),
        session_ref: Some("x".into()),
        turn_boundary: InstructionTurnBoundary {
            message_id: Some("m".into()),
        },
        reason: InstructionApplicationReason::ForkInherit,
        result: InstructionApplicationResult::Accepted,
        idempotency_key: "op".into(),
        applied_hlc: "hlc".into(),
        created_by: Some(user()),
        created_at: 6,
    };
    round_trip(
        &application,
        json!({
            "id":"a","spaceId":"s","chatId":"c","workItemId":"w","revisionId":"r",
            "sessionRef":"x","turnBoundary":{"messageId":"m"},"reason":"forkInherit",
            "result":"accepted","idempotencyKey":"op","appliedHlc":"hlc",
            "createdBy":user_json(),"createdAt":6
        }),
    );
    for (wire, reason) in [
        ("launch", InstructionApplicationReason::Launch),
        ("explicit", InstructionApplicationReason::Explicit),
        ("forkInherit", InstructionApplicationReason::ForkInherit),
        ("fork-inherit", InstructionApplicationReason::ForkInherit),
        ("keepSession", InstructionApplicationReason::KeepSession),
        ("future", InstructionApplicationReason::Explicit),
    ] {
        assert_eq!(
            serde_json::from_value::<InstructionApplicationReason>(json!(wire)).unwrap(),
            reason
        );
    }
    assert_eq!(
        serde_json::from_value::<InstructionApplicationResult>(json!("success")).unwrap(),
        InstructionApplicationResult::Accepted
    );

    round_trip(
        &ChatInstructionState {
            chat_id: "c".into(),
            selected_revision_id: Some("r".into()),
            selected_revision_hash: Some("h".into()),
            selected_revision_integrity: Some(SelectedRevisionIntegrity::Ok),
            current_effective_hash: Some("h2".into()),
            drift: true,
            delivered_revision_id: Some("r0".into()),
            delivered_revision_hash: Some("h0".into()),
        },
        json!({
            "chatId":"c","selectedRevisionId":"r","selectedRevisionHash":"h",
            "selectedRevisionIntegrity":"ok","currentEffectiveHash":"h2","drift":true,"deliveredRevisionId":"r0",
            "deliveredRevisionHash":"h0"
        }),
    );
    round_trip(
        &ChatInstructionState {
            chat_id: "c".into(),
            ..Default::default()
        },
        json!({"chatId":"c","drift":false}),
    );
    for (wire, integrity) in [
        ("ok", SelectedRevisionIntegrity::Ok),
        ("missing", SelectedRevisionIntegrity::Missing),
        ("corrupt", SelectedRevisionIntegrity::Corrupt),
        ("fromTheFuture", SelectedRevisionIntegrity::Corrupt),
    ] {
        assert_eq!(
            serde_json::from_value::<SelectedRevisionIntegrity>(json!(wire)).unwrap(),
            integrity
        );
    }
}

#[test]
fn delivery_wire_names_are_pinned() {
    let envelope = InstructionEnvelope {
        revision_id: "r".into(),
        revision_hash: "h".into(),
        envelope_version: 1,
        envelope_sha256: "e".into(),
        rendered: "text".into(),
    };
    let envelope_json = json!({
        "revisionId":"r","revisionHash":"h","envelopeVersion":1,"envelopeSha256":"e",
        "rendered":"text"
    });
    round_trip(&envelope, envelope_json.clone());
    round_trip(&InstructionTransition::Fresh, json!({"kind":"fresh"}));
    round_trip(&InstructionTransition::Continue, json!({"kind":"continue"}));
    round_trip(
        &InstructionTransition::Replace {
            previous_revision_id: "r0".into(),
        },
        json!({"kind":"replace","previousRevisionId":"r0"}),
    );
    let delivery = InstructionDelivery {
        envelope,
        transition: InstructionTransition::Fresh,
    };
    let delivery_json = json!({"envelope":envelope_json,"transition":{"kind":"fresh"}});
    round_trip(&delivery, delivery_json.clone());
    round_trip(
        &InstructionRevisionPin {
            revision_id: "r".into(),
            revision_hash: "h".into(),
        },
        json!({"revisionId":"r","revisionHash":"h"}),
    );
    round_trip(
        &ExecutionSessionReport {
            id: "x".into(),
            native_resume_id: Some("n".into()),
            harness: HarnessId::ClaudeCode,
            execution_root: "/repo".into(),
            revision_id: Some("r".into()),
            revision_hash: Some("h".into()),
        },
        json!({
            "id":"x","nativeResumeId":"n","harness":"claude-code","executionRoot":"/repo",
            "revisionId":"r","revisionHash":"h"
        }),
    );

    // RunRequest: absent instructions are omitted and default to None.
    let minimal = json!({
        "prompt":"p","model":null,"reasoning":null,"cwd":"/repo","sandbox":"workspace-write",
        "resume":null
    });
    let mut request: crate::RunRequest = serde_json::from_value(minimal).unwrap();
    assert!(request.instructions.is_none());
    assert!(
        serde_json::to_value(&request)
            .unwrap()
            .get("instructions")
            .is_none()
    );
    request.instructions = Some(delivery);
    let value = serde_json::to_value(&request).unwrap();
    assert_eq!(value["instructions"], delivery_json);
    let back: crate::RunRequest = serde_json::from_value(value).unwrap();
    assert_eq!(back, request);
}

#[test]
fn comparison_wire_names_are_pinned() {
    round_trip(
        &TextDiffLine {
            kind: TextDiffKind::Removed,
            text: "x".into(),
        },
        json!({"kind":"removed","text":"x"}),
    );
    round_trip(&TextDiffKind::Added, json!("added"));
    round_trip(&TextDiffKind::Same, json!("same"));
    round_trip(
        &InstructionRevisionComparison {
            source_hash: "a".into(),
            target_hash: "b".into(),
            identical: false,
            added: Vec::new(),
            removed: Vec::new(),
            changed: vec![InstructionTextChange {
                instruction_id: "i".into(),
                before_text: "B".into(),
                after_text: "A".into(),
            }],
            reordered: vec!["j".into()],
            line_diff: Vec::new(),
        },
        json!({
            "sourceHash":"a","targetHash":"b","identical":false,"added":[],"removed":[],
            "changed":[{"instructionId":"i","beforeText":"B","afterText":"A"}],
            "reordered":["j"],"lineDiff":[]
        }),
    );
}

#[test]
fn instruction_mutation_wire_names_are_pinned() {
    use crate::work_outline::*;
    let cases = vec![
        (
            WorkOutlineMutation::CreateWorkItemInstruction(CreateWorkItemInstructionParams {
                space_id: "s".into(),
                scope: InstructionScope::WorkItem {
                    work_item_id: "w".into(),
                },
                text: "T".into(),
                placement: Some(InstructionPlacement::Before {
                    sibling_instruction_id: "i0".into(),
                }),
                source: Some(InstructionSource {
                    chat_id: Some("c".into()),
                    ..Default::default()
                }),
                operation_id: "op".into(),
            }),
            json!({
                "op":"createWorkItemInstruction","spaceId":"s",
                "scope":{"type":"workItem","workItemId":"w"},"text":"T",
                "placement":{"type":"before","siblingInstructionId":"i0"},
                "source":{"chatId":"c"},"operationId":"op"
            }),
        ),
        (
            WorkOutlineMutation::CreateWorkItemInstruction(CreateWorkItemInstructionParams {
                space_id: "s".into(),
                scope: InstructionScope::SpaceRoot,
                text: "T".into(),
                placement: None,
                source: None,
                operation_id: "op".into(),
            }),
            json!({
                "op":"createWorkItemInstruction","spaceId":"s","scope":{"type":"spaceRoot"},
                "text":"T","operationId":"op"
            }),
        ),
        (
            WorkOutlineMutation::UpdateWorkItemInstruction(UpdateWorkItemInstructionParams {
                instruction_id: "i".into(),
                text: "T2".into(),
                operation_id: "op".into(),
            }),
            json!({"op":"updateWorkItemInstruction","instructionId":"i","text":"T2","operationId":"op"}),
        ),
        (
            WorkOutlineMutation::MoveWorkItemInstruction(MoveWorkItemInstructionParams {
                instruction_id: "i".into(),
                placement: InstructionPlacement::After {
                    sibling_instruction_id: "i1".into(),
                },
                operation_id: "op".into(),
            }),
            json!({
                "op":"moveWorkItemInstruction","instructionId":"i",
                "placement":{"type":"after","siblingInstructionId":"i1"},"operationId":"op"
            }),
        ),
        (
            WorkOutlineMutation::ArchiveWorkItemInstruction(ArchiveWorkItemInstructionParams {
                instruction_id: "i".into(),
                operation_id: "op".into(),
            }),
            json!({"op":"archiveWorkItemInstruction","instructionId":"i","operationId":"op"}),
        ),
        (
            WorkOutlineMutation::CreateInstructionRevision(CreateInstructionRevisionParams {
                work_item_id: "w".into(),
                operation_id: "op".into(),
            }),
            json!({"op":"createInstructionRevision","workItemId":"w","operationId":"op"}),
        ),
        (
            WorkOutlineMutation::ApplyInstructionRevision(ApplyInstructionRevisionParams {
                chat_id: "c".into(),
                revision_id: "r".into(),
                reason: InstructionApplicationReason::Explicit,
                turn_boundary: Some(InstructionTurnBoundary {
                    message_id: Some("m".into()),
                }),
                operation_id: "op".into(),
            }),
            json!({
                "op":"applyInstructionRevision","chatId":"c","revisionId":"r","reason":"explicit",
                "turnBoundary":{"messageId":"m"},"operationId":"op"
            }),
        ),
    ];
    for (mutation, expected) in cases {
        assert!(mutation.is_instruction_operation());
        assert_eq!(mutation.operation_id(), "op");
        round_trip(&mutation, expected);
    }
    round_trip(&InstructionPlacement::First, json!({"type":"first"}));
    round_trip(&InstructionPlacement::Last, json!({"type":"last"}));

    // Replies carry exactly the minted id.
    for (reply, expected) in [
        (
            WorkOutlineMutateReply {
                instruction_id: Some(derive_id("op", roles::INSTRUCTION)),
                ..Default::default()
            },
            json!({"instructionId": derive_id("op", "instruction")}),
        ),
        (
            WorkOutlineMutateReply {
                revision_id: Some(derive_id("op", roles::REVISION)),
                ..Default::default()
            },
            json!({"revisionId": derive_id("op", "revision")}),
        ),
        (
            WorkOutlineMutateReply {
                application_id: Some(derive_id("op", roles::APPLICATION)),
                ..Default::default()
            },
            json!({"applicationId": derive_id("op", "application")}),
        ),
    ] {
        round_trip(&reply, expected);
    }
    assert_eq!(
        WorkOutlineMutateReply::default()
            .into_revision_id()
            .unwrap_err()
            .code,
        WorkOutlineErrorCode::Invalid
    );
}

#[test]
fn query_wire_names_are_pinned() {
    use crate::work_outline::WorkOutlineQueryResponse;
    round_trip(
        &WorkOutlineQueryRequest::ResolveEffectiveInstructions {
            work_item_id: "w".into(),
        },
        json!({"query":"resolveEffectiveInstructions","workItemId":"w"}),
    );
    round_trip(
        &WorkOutlineQueryRequest::CompareInstructionRevisions {
            source_revision_id: "a".into(),
            target_revision_id: "b".into(),
        },
        json!({"query":"compareInstructionRevisions","sourceRevisionId":"a","targetRevisionId":"b"}),
    );
    round_trip(
        &WorkOutlineQueryRequest::GetInstructionRevision {
            revision_id: "r".into(),
        },
        json!({"query":"getInstructionRevision","revisionId":"r"}),
    );
    // Strict: an unknown query is an error, not a silent default.
    assert!(serde_json::from_value::<WorkOutlineQueryRequest>(json!({"query":"nope"})).is_err());

    let (items, instructions) = fixture(true, false);
    let effective = resolve_effective_instructions_in("w-leaf", &items, &instructions);
    let response = WorkOutlineQueryResponse::ResolveEffectiveInstructions(effective.clone());
    let value = serde_json::to_value(&response).unwrap();
    assert_eq!(value["query"], "resolveEffectiveInstructions");
    assert_eq!(value["workItemId"], "w-leaf");
    assert_eq!(
        serde_json::from_value::<WorkOutlineQueryResponse>(value).unwrap(),
        response
    );

    round_trip(
        &WorkOutlineQueryResponse::CompareInstructionRevisions {
            source_revision_id: "a".into(),
            target_revision_id: "b".into(),
            comparison: InstructionRevisionComparison::default(),
        },
        json!({
            "query":"compareInstructionRevisions","sourceRevisionId":"a","targetRevisionId":"b",
            "comparison":{"sourceHash":"","targetHash":"","identical":false,"added":[],
                          "removed":[],"changed":[],"reordered":[],"lineDiff":[]}
        }),
    );
    let revision = InstructionRevisionView {
        id: "r".into(),
        space_id: "s".into(),
        work_item_id: "w".into(),
        body: build_revision_body(Vec::new()).unwrap(),
        created_by: None,
        created_at: 0,
        integrity: InstructionRevisionIntegrity::Ok,
    };
    let value = serde_json::to_value(WorkOutlineQueryResponse::GetInstructionRevision(
        revision.clone(),
    ))
    .unwrap();
    assert_eq!(value["query"], "getInstructionRevision");
    assert_eq!(value["id"], "r");
    assert_eq!(value["integrity"], "ok");
}

#[test]
fn snapshot_carries_instruction_state_in_same_content_as() {
    let (items, instructions) = fixture(true, true);
    let base = crate::WorkOutlineSnapshot {
        space_id: "s-1".into(),
        items,
        instructions,
        ..Default::default()
    };
    let mut changed = base.clone();
    changed.instructions[0].text.push('!');
    assert!(!base.same_content_as(&changed));
    let mut state = base.clone();
    state.chat_instruction_states.push(ChatInstructionState {
        chat_id: "c".into(),
        drift: true,
        ..Default::default()
    });
    assert!(!base.same_content_as(&state));
    assert_eq!(
        state.chat_instruction_state("c").map(|s| s.drift),
        Some(true)
    );
    let mut revisions = base.clone();
    revisions.revisions.push(InstructionRevisionView {
        id: "r".into(),
        space_id: "s-1".into(),
        work_item_id: "w-leaf".into(),
        body: build_revision_body(Vec::new()).unwrap(),
        created_by: None,
        created_at: 0,
        integrity: InstructionRevisionIntegrity::Ok,
    });
    assert!(!base.same_content_as(&revisions));
    assert!(revisions.revision("r").is_some());
}

#[test]
fn delivery_outcomes_have_stable_wire_shapes() {
    let reset = InstructionDeliveryRejection {
        outcome: InstructionDeliveryOutcome::InstructionContextResetRequired {
            harness: HarnessId::Codex,
            message_id: Some("m-2".into()),
            revision_id: "rev-b".into(),
            revision_hash: "hb".into(),
            previous_revision_id: Some("rev-a".into()),
        },
        message: "confirm".into(),
    };
    round_trip(
        &reset,
        json!({
            "code": "instructionContextResetRequired",
            "harness": "codex",
            "messageId": "m-2",
            "revisionId": "rev-b",
            "revisionHash": "hb",
            "previousRevisionId": "rev-a",
            "message": "confirm"
        }),
    );
    assert_eq!(reset.outcome.code(), INSTRUCTION_CONTEXT_RESET_REQUIRED);
    let wire = reset.to_wire_message();
    assert!(wire.starts_with("instruction-delivery/{"));
    assert_eq!(
        InstructionDeliveryRejection::from_wire_message(&wire),
        Some(reset)
    );
    assert_eq!(
        InstructionDeliveryRejection::from_wire_message("unknown method: X"),
        None
    );

    let host = InstructionDeliveryOutcome::InstructionHostUnsupported {
        host_device_id: "d2".into(),
        harness: HarnessId::ClaudeCode,
        required_capability: "work-outline-instruction-envelope-v1/claude-code".into(),
        reason: InstructionHostUnsupportedReason::ZeronUpgradeRequired,
        device_name: Some("Studio".into()),
        device_version: None,
    };
    round_trip(
        &host,
        json!({
            "code": "instructionHostUnsupported",
            "hostDeviceId": "d2",
            "harness": "claude-code",
            "requiredCapability": "work-outline-instruction-envelope-v1/claude-code",
            "reason": "zeronUpgradeRequired",
            "deviceName": "Studio"
        }),
    );
}
