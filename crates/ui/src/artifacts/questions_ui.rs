//! Pure decisions behind the plan question UI: the questions summary and
//! drift badge, the throttle for drift fetches, the "changed since you last
//! viewed" marker, and what an answer sends to the publishing chat.
//!
//! No LLM and no I/O here: inputs in, labels out (design doc, C and D).
use std::collections::HashMap;
use std::time::{Duration, Instant};

use zeron_proto::artifacts::{open_unanswered, unreflected_answers};
use zeron_proto::{ArtifactDriftReply, ArtifactQuestionAnswer, ArtifactView, PlanQuestion};

use crate::work_outline::required_actions::reply_message;

/// Drift is re-asked at most this often per artifact.
pub(crate) const DRIFT_MIN_INTERVAL: Duration = Duration::from_secs(60);

/// A small badge: a short label and the why, shown as a tooltip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DriftBadge {
    pub(crate) label: String,
    pub(crate) tooltip: String,
}

/// "N open questions" for the header, `None` when nothing is open.
pub(crate) fn open_questions_label(
    questions: &[PlanQuestion],
    answers: &[ArtifactQuestionAnswer],
) -> Option<String> {
    match open_unanswered(questions, answers).len() {
        0 => None,
        1 => Some("1 open question".to_owned()),
        n => Some(format!("{n} open questions")),
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// "3d", "5h", "12m", "just now": coarse age of `at_ms` as of `now_ms`.
pub(crate) fn age_label(at_ms: i64, now_ms: i64) -> String {
    let secs = ((now_ms - at_ms) / 1000).max(0);
    match secs {
        0..=59 => "just now".to_owned(),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86_399 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86_400),
    }
}

/// The drift badge: answered questions the plan does not reflect yet, and/or
/// commits made since the latest revision. `None` when neither holds.
pub(crate) fn drift_badge(
    questions: &[PlanQuestion],
    answers: &[ArtifactQuestionAnswer],
    latest_revision: u32,
    latest_revision_at: i64,
    drift: Option<&ArtifactDriftReply>,
    now_ms: i64,
) -> Option<DriftBadge> {
    let mut labels = Vec::new();
    let mut tips = Vec::new();
    let unreflected = unreflected_answers(questions, answers, latest_revision_at).len();
    if unreflected > 0 {
        labels.push(format!(
            "{} not in plan",
            plural(unreflected, "answer", "answers")
        ));
        tips.push(format!(
            "{} not yet reflected in the plan",
            plural(unreflected, "answered question", "answered questions"),
        ));
    }
    if let Some(drift) = drift.filter(|drift| drift.commits_since > 0) {
        let commits = drift.commits_since as usize;
        labels.push(format!(
            "{} since r{latest_revision}",
            plural(commits, "commit", "commits")
        ));
        let branch = drift
            .branch
            .as_deref()
            .filter(|branch| !branch.is_empty())
            .map(|branch| format!(" on {branch}"))
            .unwrap_or_default();
        let since = if drift.since > 0 {
            drift.since
        } else {
            latest_revision_at
        };
        tips.push(format!(
            "{}{branch} since rev {latest_revision} ({})",
            plural(commits, "commit", "commits"),
            age_label(since, now_ms),
        ));
    }
    (!labels.is_empty()).then(|| DriftBadge {
        label: labels.join(" \u{b7} "),
        tooltip: tips.join("\n"),
    })
}

/// Throttle for drift fetches: once when the tab opens and once per
/// revision change, never more often than every [`DRIFT_MIN_INTERVAL`] (a
/// change inside the window is picked up by the next check after it).
#[derive(Debug, Default)]
pub(crate) struct DriftGate {
    last: Option<(u32, Instant)>,
}

impl DriftGate {
    /// Whether to fetch now; records the attempt when true.
    pub(crate) fn due(&mut self, revision: u32, now: Instant) -> bool {
        let due = match self.last {
            None => true,
            Some((last, at)) => last != revision && now.duration_since(at) >= DRIFT_MIN_INTERVAL,
        };
        if due {
            self.last = Some((revision, now));
        }
        due
    }
}

/// `(from, to)` when the artifact moved on since it was last viewed. First
/// visits (nothing recorded) and unchanged revisions read `None`.
pub(crate) fn changed_since(last_viewed: Option<u32>, current: u32) -> Option<(u32, u32)> {
    last_viewed
        .filter(|&last| last > 0 && last < current)
        .map(|last| (last, current))
}

/// The chip text: "Updated since you last viewed · rev 2 → 4".
pub(crate) fn changed_since_label(from: u32, to: u32) -> String {
    format!("Updated since you last viewed \u{b7} rev {from} \u{2192} {to}")
}

/// The chat an answer is delivered to: the newest revision's publisher, else
/// the chat that first published the artifact.
pub(crate) fn answer_target_chat(view: &ArtifactView) -> Option<String> {
    view.current()
        .and_then(|revision| revision.by.chat_id.clone())
        .or_else(|| view.chat_id.clone())
        .filter(|chat| !chat.is_empty())
}

/// `Re: "Q-3: <question>": <answer>`, built like Reply & resolve's message.
pub(crate) fn answer_message(id: &str, question_text: &str, answer: &str) -> String {
    reply_message(&format!("{id}: {question_text}"), answer)
}

/// The recorded answers as `upper-cased id -> text`, for the cards.
pub(crate) fn answer_texts(view: &ArtifactView) -> HashMap<String, String> {
    view.answers
        .iter()
        .map(|answer| (answer.id.to_ascii_uppercase(), answer.answer.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeron_proto::{ArtifactKind, PlanQuestionStatus, WorkOutlineActor, extract_questions};

    const MD: &str = "> [!question] Q-1 · One?\n> status: open\n\n> [!question] Q-2 · Two?\n> status: open\n\n> [!question] Q-3 · Three?\n> status: resolved\n> decision: x\n";

    fn answer(id: &str, at: i64) -> ArtifactQuestionAnswer {
        ArtifactQuestionAnswer {
            id: id.into(),
            answer: "yes".into(),
            at,
            ..Default::default()
        }
    }

    fn drift(commits: u32) -> ArtifactDriftReply {
        ArtifactDriftReply {
            artifact_id: "a".into(),
            commits_since: commits,
            branch: Some("feat/x".into()),
            latest_commit_at: Some(9),
            since: 1_000,
        }
    }

    #[test]
    fn open_label_counts_unanswered_open_questions() {
        let qs = extract_questions(ArtifactKind::Markdown, MD);
        assert_eq!(
            open_questions_label(&qs, &[]).as_deref(),
            Some("2 open questions")
        );
        assert_eq!(
            open_questions_label(&qs, &[answer("q-1", 5)]).as_deref(),
            Some("1 open question")
        );
        assert_eq!(
            open_questions_label(&qs, &[answer("Q-1", 5), answer("Q-2", 5)]),
            None
        );
        assert!(qs.iter().any(|q| q.status == PlanQuestionStatus::Resolved));
    }

    #[test]
    fn badge_for_unreflected_answers_only() {
        let qs = extract_questions(ArtifactKind::Markdown, MD);
        // Q-1 answered after the revision; Q-3 is resolved so it is ignored;
        // Q-2 answered before the revision is already reflected.
        let answers = [
            answer("Q-1", 2_000),
            answer("Q-3", 2_000),
            answer("Q-2", 500),
        ];
        let badge = drift_badge(&qs, &answers, 4, 1_000, None, 5_000).unwrap();
        assert_eq!(badge.label, "1 answer not in plan");
        assert_eq!(
            badge.tooltip,
            "1 answered question not yet reflected in the plan"
        );
    }

    #[test]
    fn badge_for_commits_only() {
        let badge =
            drift_badge(&[], &[], 3, 1_000, Some(&drift(2)), 1_000 + 3 * 86_400_000).unwrap();
        assert_eq!(badge.label, "2 commits since r3");
        assert_eq!(badge.tooltip, "2 commits on feat/x since rev 3 (3d ago)");
    }

    #[test]
    fn badge_for_both_and_none() {
        let qs = extract_questions(ArtifactKind::Markdown, MD);
        let answers = [answer("Q-1", 2_000), answer("Q-2", 2_000)];
        let both = drift_badge(&qs, &answers, 2, 1_000, Some(&drift(1)), 2_000).unwrap();
        assert_eq!(both.label, "2 answers not in plan \u{b7} 1 commit since r2");
        assert_eq!(both.tooltip.lines().count(), 2);
        assert_eq!(
            drift_badge(&qs, &[], 2, 1_000, Some(&drift(0)), 2_000),
            None
        );
        assert_eq!(drift_badge(&[], &[], 2, 1_000, None, 2_000), None);
    }

    #[test]
    fn drift_gate_throttles_per_minute() {
        let mut gate = DriftGate::default();
        let t0 = Instant::now();
        assert!(gate.due(1, t0));
        // Same revision: never again.
        assert!(!gate.due(1, t0 + Duration::from_secs(61)));
        // A new revision inside the minute waits for the window.
        assert!(!gate.due(2, t0 + Duration::from_secs(10)));
        assert!(gate.due(2, t0 + Duration::from_secs(61)));
    }

    #[test]
    fn changed_since_needs_an_earlier_view() {
        assert_eq!(changed_since(None, 4), None);
        assert_eq!(changed_since(Some(4), 4), None);
        assert_eq!(changed_since(Some(2), 4), Some((2, 4)));
        assert_eq!(changed_since(Some(5), 4), None);
        assert_eq!(
            changed_since_label(2, 4),
            "Updated since you last viewed \u{b7} rev 2 \u{2192} 4"
        );
    }

    #[test]
    fn answer_goes_to_the_latest_publisher_else_the_first() {
        let mut view = ArtifactView {
            chat_id: Some("first".into()),
            current_revision: 2,
            revisions: vec![zeron_proto::ArtifactRevision {
                id: 2,
                by: WorkOutlineActor {
                    chat_id: Some("latest".into()),
                    ..Default::default()
                },
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(answer_target_chat(&view).as_deref(), Some("latest"));
        view.revisions[0].by.chat_id = None;
        assert_eq!(answer_target_chat(&view).as_deref(), Some("first"));
        view.chat_id = None;
        assert_eq!(answer_target_chat(&view), None);
    }

    #[test]
    fn answer_message_quotes_the_question() {
        assert_eq!(
            answer_message("Q-3", "Merge by  MPN?", " by MPN "),
            "Re: \"Q-3: Merge by MPN?\": by MPN"
        );
    }
}
