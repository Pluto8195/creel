//! Edit / remove instruction proposals: the pure, GUI-free parts of their
//! review. An inline word diff against the current text, the stale test
//! (three-way: the proposal's base, the instruction now, the proposed text),
//! and grouping of pending proposals that touch the same instruction.

use mast_proto::{
    InstructionProposalStatus, ProposalAction, WorkItemInstructionProposalView,
    WorkItemInstructionView, WorkOutlineSnapshot,
};
use similar::{ChangeTag, TextDiff};

/// One run of an inline diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiffKind {
    Same,
    Added,
    Removed,
}

/// Word diff of `old` → `new`. Adjacent runs of the same kind are merged and
/// whitespace stays attached to its words, so the runs concatenate to the
/// `old + added` / `new + removed` texts.
pub(crate) fn word_diff(old: &str, new: &str) -> Vec<(DiffKind, String)> {
    let mut runs: Vec<(DiffKind, String)> = Vec::new();
    for change in TextDiff::from_words(old, new).iter_all_changes() {
        let kind = match change.tag() {
            ChangeTag::Equal => DiffKind::Same,
            ChangeTag::Insert => DiffKind::Added,
            ChangeTag::Delete => DiffKind::Removed,
        };
        match runs.last_mut() {
            Some((last, text)) if *last == kind => text.push_str(change.value()),
            _ => runs.push((kind, change.value().to_owned())),
        }
    }
    runs
}

/// The text a diff shows and the highlight ranges (byte offsets) over it:
/// added runs and removed runs (a removal is shown struck through, in place).
pub(crate) fn diff_layout(
    runs: &[(DiffKind, String)],
) -> (String, Vec<(std::ops::Range<usize>, DiffKind)>) {
    let mut text = String::new();
    let mut ranges = Vec::new();
    for (kind, run) in runs {
        let start = text.len();
        text.push_str(run);
        if *kind != DiffKind::Same {
            ranges.push((start..text.len(), *kind));
        }
    }
    (text, ranges)
}

/// The instruction a pending edit / remove targets, when it still exists.
pub(crate) fn target_instruction<'a>(
    snapshot: &'a WorkOutlineSnapshot,
    proposal: &WorkItemInstructionProposalView,
) -> Option<&'a WorkItemInstructionView> {
    let id = proposal.instruction_id.as_deref()?;
    snapshot.instructions.iter().find(|i| i.id == id)
}

/// How a pending edit / remove stands against its instruction now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Standing {
    /// An `add`, or an edit / remove whose instruction is unchanged.
    Current,
    /// The instruction's text changed since the proposal's base (or it was
    /// archived): `now` is its text today, `None` once archived or gone.
    Stale { now: Option<String> },
}

pub(crate) fn standing(
    snapshot: &WorkOutlineSnapshot,
    proposal: &WorkItemInstructionProposalView,
) -> Standing {
    if proposal.action == ProposalAction::Add {
        return Standing::Current;
    }
    match target_instruction(snapshot, proposal) {
        Some(instruction) if !proposal.is_stale_against(instruction) => Standing::Current,
        Some(instruction) if !instruction.archived() => Standing::Stale {
            now: Some(instruction.text.clone()),
        },
        _ => Standing::Stale { now: None },
    }
}

/// Pending proposals that belong together: edits and removes of one
/// instruction form a group, and every `add` stands alone. Groups keep the
/// order of their first proposal.
#[derive(Debug, Clone)]
pub(crate) struct ProposalGroup {
    pub instruction_id: Option<String>,
    pub proposals: Vec<WorkItemInstructionProposalView>,
}

pub(crate) fn group_pending(
    proposals: impl IntoIterator<Item = WorkItemInstructionProposalView>,
) -> Vec<ProposalGroup> {
    let mut groups: Vec<ProposalGroup> = Vec::new();
    for proposal in proposals {
        if proposal.status != InstructionProposalStatus::Pending {
            continue;
        }
        let key = (proposal.action != ProposalAction::Add)
            .then(|| proposal.instruction_id.clone())
            .flatten();
        match key.as_ref().and_then(|k| {
            groups
                .iter_mut()
                .find(|g| g.instruction_id.as_ref() == Some(k))
        }) {
            Some(group) => group.proposals.push(proposal),
            None => groups.push(ProposalGroup {
                instruction_id: key,
                proposals: vec![proposal],
            }),
        }
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proposal(
        id: &str,
        action: ProposalAction,
        instruction: Option<&str>,
    ) -> WorkItemInstructionProposalView {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "action": action.as_str(),
            "instructionId": instruction,
            "text": "t",
        }))
        .unwrap()
    }

    #[test]
    fn word_diff_marks_added_and_removed_words_in_place() {
        let runs = word_diff("Run the linter first", "Run the linter and tests first");
        let (text, ranges) = diff_layout(&runs);
        assert_eq!(text, "Run the linter and tests first");
        assert_eq!(ranges.len(), 1);
        assert_eq!(&text[ranges[0].0.clone()], "and tests ");
        assert_eq!(ranges[0].1, DiffKind::Added);

        let (text, ranges) =
            diff_layout(&word_diff("Use tabs everywhere", "Use spaces everywhere"));
        let removed: Vec<_> = ranges
            .iter()
            .filter(|(_, k)| *k == DiffKind::Removed)
            .map(|(r, _)| &text[r.clone()])
            .collect();
        assert_eq!(removed, ["tabs"]);
        assert!(
            ranges
                .iter()
                .any(|(r, k)| *k == DiffKind::Added && &text[r.clone()] == "spaces")
        );
        // Identical text has no highlights.
        assert!(diff_layout(&word_diff("same", "same")).1.is_empty());
    }

    #[test]
    fn groups_collect_modifications_of_one_instruction_and_keep_adds_alone() {
        let groups = group_pending([
            proposal("a", ProposalAction::Edit, Some("i-1")),
            proposal("b", ProposalAction::Add, None),
            proposal("c", ProposalAction::Remove, Some("i-1")),
            proposal("d", ProposalAction::Edit, Some("i-2")),
            proposal("e", ProposalAction::Add, None),
        ]);
        let shape: Vec<(Option<&str>, Vec<&str>)> = groups
            .iter()
            .map(|g| {
                (
                    g.instruction_id.as_deref(),
                    g.proposals.iter().map(|p| p.id.as_str()).collect(),
                )
            })
            .collect();
        assert_eq!(
            shape,
            [
                (Some("i-1"), vec!["a", "c"]),
                (None, vec!["b"]),
                (Some("i-2"), vec!["d"]),
                (None, vec!["e"]),
            ]
        );
    }
}
