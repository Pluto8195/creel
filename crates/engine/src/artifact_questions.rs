//! Plan questions → Required Actions (Artifacts phase 3 D).
//!
//! `ArtifactQuestionSync::reconcile` runs on the artifact's owning device
//! (every [`INTERVAL`], and right after an answer) and keeps the work item's
//! Required Actions in step with the plan's open questions:
//!
//! - a **blocking** open question owns a request, dedupe key
//!   `artq:<artifactId>:<Q-n>`, text `Q-n: <question>`;
//! - all other open questions roll up into one request, key
//!   `artq:<artifactId>:rollup`, text `N open questions in <title>`, whose
//!   `info` lists them one per line (`Q-n: text`). The request cannot be
//!   edited, so a changed list resolves the old one ("Superseded") and
//!   raises a new one;
//! - a question that is answered on the row, resolved or removed in the
//!   document, or no longer open resolves its request (by the system actor);
//! - a request a *person* resolved (Required Actions' Resolve or Reply &
//!   resolve) records an answer on the artifact (the resolution note, else
//!   "Resolved in Required Actions"): a blocking request answers its
//!   question, the roll-up answers every question its `info` lists.
//!
//! The origin Chat is the Chat that published the latest revision (else the
//! one that created the artifact) so Reply & resolve reaches it; without a
//! live Chat the request is system-origin. Operation ids derive from
//! (artifact, question, revision) so a replay creates nothing twice.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use zeron_proto::artifacts::{ArtifactQuestionAnswer, PlanQuestion, PlanQuestionStatus};
use zeron_proto::work_outline::{WorkOutlineActorKind, derive_id};
use zeron_proto::work_outline_user_actions::{
    CreateUserActionRequestParams, ResolveUserActionRequestParams, UserActionOriginKind,
    UserActionRequestOrigin, UserActionRequestView, user_action_limits,
};
use zeron_proto::{ArtifactView, WorkOutlineMutation};

use crate::artifacts::ArtifactsService;
use crate::work_outline::WorkOutlineService;

pub const INTERVAL: Duration = Duration::from_secs(2);
const KEY_PREFIX: &str = "artq:";
const RESOLVED_IN_ACTIONS: &str = "Resolved in Required Actions";

#[derive(Clone)]
pub struct ArtifactQuestionSync {
    artifacts: ArtifactsService,
    outline: Arc<WorkOutlineService>,
}

pub fn question_key(artifact_id: &str, question_id: &str) -> String {
    format!("{KEY_PREFIX}{artifact_id}:{question_id}")
}

pub fn rollup_key(artifact_id: &str) -> String {
    question_key(artifact_id, "rollup")
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn question_line(q: &PlanQuestion) -> String {
    format!("{}: {}", q.id, one_line(&q.text))
}

/// `Q-n` ids at the start of each `info` line of a roll-up request.
fn ids_in_info(info: &str) -> Vec<String> {
    info.lines()
        .filter_map(|line| {
            let id = line.split(':').next()?.trim();
            (id.len() > 2 && id[..2].eq_ignore_ascii_case("q-")).then(|| id.to_ascii_uppercase())
        })
        .collect()
}

impl ArtifactQuestionSync {
    pub fn new(artifacts: ArtifactsService) -> Self {
        let outline = Arc::new(WorkOutlineService::new(artifacts.workspace().clone()));
        Self { artifacts, outline }
    }

    /// One pass over this device's plan artifacts. Returns the number of
    /// Required Action writes (create or resolve) plus recorded answers.
    pub fn reconcile_all(&self) -> usize {
        let ws = self.artifacts.workspace();
        let mut changes = 0;
        for artifact in ws.all_artifacts() {
            if artifact.device_id != self.artifacts.device_id() {
                continue;
            }
            changes += self.reconcile(&artifact.id);
        }
        changes
    }

    pub fn spawn_loop(&self) {
        let sync = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                let sync = sync.clone();
                let _ = tokio::task::spawn_blocking(move || sync.reconcile_all()).await;
            }
        });
    }

    /// Reconcile one artifact; see the module docs.
    pub fn reconcile(&self, artifact_id: &str) -> usize {
        let ws = self.artifacts.workspace();
        let Some(artifact) = ws.artifact(artifact_id) else {
            return 0;
        };
        let Some(item) = artifact.work_item_id.clone() else {
            return 0;
        };
        if artifact.device_id != self.artifacts.device_id()
            || artifact.archived_at.is_some()
            || !(artifact.pinned_as_plan || artifact.role.as_deref() == Some("plan"))
        {
            return 0;
        }
        let questions = self.artifacts.plan_questions(&artifact);
        let snapshot = self.outline.snapshot(&artifact.space_id);
        let prefix = format!("{KEY_PREFIX}{}:", artifact.id);
        let requests: Vec<&UserActionRequestView> = snapshot
            .user_action_requests
            .iter()
            .filter(|r| r.work_item_id == item)
            .filter(|r| r.dedupe_key.as_deref().is_some_and(|k| k.starts_with(&prefix)))
            .collect();
        if questions.is_empty() && requests.is_empty() {
            return 0;
        }
        let mut changes = 0;

        // 1. A person's resolution answers the question(s).
        let mut answers = artifact.answers.clone();
        let mut answered_now: Vec<ArtifactQuestionAnswer> = Vec::new();
        for request in requests.iter().filter(|r| !r.is_open()) {
            let Some(resolution) = request.resolution.as_ref() else {
                continue;
            };
            if resolution.by.kind == WorkOutlineActorKind::System {
                continue;
            }
            let key = request.dedupe_key.clone().unwrap_or_default();
            let ids: Vec<String> = if key == rollup_key(&artifact.id) {
                ids_in_info(request.info.as_deref().unwrap_or(""))
            } else {
                vec![key[prefix.len()..].to_ascii_uppercase()]
            };
            let note = resolution
                .note
                .as_deref()
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .unwrap_or(RESOLVED_IN_ACTIONS);
            for id in ids {
                let open_in_doc = questions
                    .iter()
                    .any(|q| q.id == id && q.status == PlanQuestionStatus::Open);
                let already = answers.iter().any(|a| a.id.eq_ignore_ascii_case(&id));
                // Only a resolution that postdates the question's current
                // revision can answer it; older ones belong to a request
                // for an earlier incarnation of the same id.
                if open_in_doc && !already && resolution.at >= first_revision_at(&artifact) {
                    let answer = ArtifactQuestionAnswer {
                        id,
                        answer: truncate(note, zeron_proto::artifacts::MAX_ANSWER_CHARS),
                        at: resolution.at,
                        by: resolution.by.clone(),
                        revision: artifact.current_revision,
                    };
                    answers.push(answer.clone());
                    answered_now.push(answer);
                }
            }
        }
        if !answered_now.is_empty() {
            let excess = answers.len().saturating_sub(zeron_proto::artifacts::MAX_ANSWERS);
            answers.drain(..excess);
            match ws.set_artifact_answers(&artifact.id, &answers) {
                Ok(_) => changes += answered_now.len(),
                Err(error) => {
                    tracing::warn!(artifact = %artifact.id, %error, "artifact answers write failed");
                    answers = artifact.answers.clone();
                }
            }
        }

        // 2. Desired state.
        let open: Vec<&PlanQuestion> = zeron_proto::artifacts::open_unanswered(&questions, &answers);
        let blocking: Vec<&PlanQuestion> = open.iter().copied().filter(|q| q.blocking).collect();
        let rollup: Vec<&PlanQuestion> = open.iter().copied().filter(|q| !q.blocking).collect();
        let origin = self.origin_for(&artifact);
        let mut wanted_keys: HashSet<String> = HashSet::new();

        for q in &blocking {
            let key = question_key(&artifact.id, &q.id);
            wanted_keys.insert(key.clone());
            if requests.iter().any(|r| r.is_open() && r.dedupe_key.as_deref() == Some(&key)) {
                continue;
            }
            let mut info = vec![format!("Plan: {} (rev {})", artifact.title, artifact.current_revision)];
            if let Some(blocks) = &q.blocks {
                info.push(format!("Blocks: {}", one_line(blocks)));
            }
            info.push(artifact.url());
            changes += self.create(
                &item,
                &origin,
                truncate(&question_line(q), user_action_limits::MAX_REQUEST_TEXT_CHARS),
                info.join("\n"),
                key,
                format!("artq\0{}\0{}\0{}", artifact.id, q.id, artifact.current_revision),
            ) as usize;
        }

        let rollup_k = rollup_key(&artifact.id);
        let rollup_info = rollup
            .iter()
            .map(|q| truncate(&question_line(q), 300))
            .collect::<Vec<_>>()
            .join("\n");
        let rollup_text = format!(
            "{} open question{} in {}",
            rollup.len(),
            if rollup.len() == 1 { "" } else { "s" },
            one_line(&artifact.title)
        );
        let rollup_info_full = format!("{rollup_info}\n{}", artifact.url());
        let rollup_info_full = truncate(&rollup_info_full, user_action_limits::MAX_REQUEST_INFO_CHARS);
        if !rollup.is_empty() {
            wanted_keys.insert(rollup_k.clone());
        }
        let mut rollup_current = false;
        for request in requests.iter().filter(|r| r.is_open()) {
            let key = request.dedupe_key.clone().unwrap_or_default();
            if key == rollup_k {
                if !rollup.is_empty()
                    && request.text == truncate(&rollup_text, user_action_limits::MAX_REQUEST_TEXT_CHARS)
                    && request.info.as_deref() == Some(rollup_info_full.as_str())
                {
                    rollup_current = true;
                    continue;
                }
                let note = if rollup.is_empty() {
                    self.why_resolved(&artifact, &questions, &answers, None)
                } else {
                    "Superseded: the list of open questions changed".to_owned()
                };
                changes += self.resolve(request, note) as usize;
            } else if !wanted_keys.contains(&key) {
                let id = &key[prefix.len()..];
                let note = self.why_resolved(&artifact, &questions, &answers, Some(id));
                changes += self.resolve(request, note) as usize;
            }
        }
        if !rollup.is_empty() && !rollup_current {
            changes += self.create(
                &item,
                &origin,
                truncate(&rollup_text, user_action_limits::MAX_REQUEST_TEXT_CHARS),
                rollup_info_full,
                rollup_k,
                format!(
                    "artq-rollup\0{}\0{}\0{}",
                    artifact.id,
                    artifact.current_revision,
                    derive_id(&rollup_info, "rollup")
                ),
            ) as usize;
        }
        changes
    }

    fn why_resolved(
        &self,
        artifact: &ArtifactView,
        questions: &[PlanQuestion],
        answers: &[ArtifactQuestionAnswer],
        id: Option<&str>,
    ) -> String {
        let Some(id) = id else {
            return "No open questions left in the plan".to_owned();
        };
        match questions.iter().find(|q| q.id == id) {
            None => format!("{id} was removed from the plan (rev {})", artifact.current_revision),
            Some(q) if q.status == PlanQuestionStatus::Resolved => {
                format!("{id} is resolved in the plan (rev {})", artifact.current_revision)
            }
            Some(_) if answers.iter().any(|a| a.id.eq_ignore_ascii_case(id)) => {
                let answer = answers.iter().find(|a| a.id.eq_ignore_ascii_case(id));
                let text = answer.map(|a| one_line(&a.answer)).unwrap_or_default();
                truncate(&format!("Answered in Mast: {text}"), user_action_limits::MAX_NOTE_CHARS)
            }
            Some(q) if q.status == PlanQuestionStatus::Answered => {
                format!("{id} is marked answered in the plan")
            }
            Some(_) => format!("{id} is no longer a blocking question"),
        }
    }

    /// The Chat that published the latest revision, when it is live and in
    /// the artifact's Space; else the artifact's first publisher; else system.
    fn origin_for(&self, artifact: &ArtifactView) -> UserActionRequestOrigin {
        let ws = self.artifacts.workspace();
        let candidates = artifact
            .revisions
            .iter()
            .filter_map(|r| r.by.chat_id.clone())
            .take(1)
            .chain(artifact.chat_id.clone());
        for chat_id in candidates {
            let live = ws.chat(&chat_id).ok().flatten().is_some_and(|c| {
                !c.archived && c.space_id.as_deref() == Some(artifact.space_id.as_str())
            });
            if live {
                return UserActionRequestOrigin {
                    kind: UserActionOriginKind::Chat,
                    chat_id,
                    message_id: None,
                    tool_call_id: None,
                };
            }
        }
        UserActionRequestOrigin {
            kind: UserActionOriginKind::System,
            chat_id: String::new(),
            message_id: None,
            tool_call_id: None,
        }
    }

    fn create(
        &self,
        item: &str,
        origin: &UserActionRequestOrigin,
        text: String,
        info: String,
        dedupe_key: String,
        operation_seed: String,
    ) -> bool {
        let mutation = WorkOutlineMutation::CreateUserActionRequest(CreateUserActionRequestParams {
            work_item_id: item.to_owned(),
            origin: origin.clone(),
            text,
            info: Some(truncate(&info, user_action_limits::MAX_REQUEST_INFO_CHARS)),
            dedupe_key: Some(truncate(&dedupe_key, user_action_limits::MAX_DEDUPE_KEY_CHARS)),
            operation_id: derive_id(&operation_seed, "artq"),
        });
        self.apply(mutation)
    }

    fn resolve(&self, request: &UserActionRequestView, note: String) -> bool {
        self.apply(WorkOutlineMutation::ResolveUserActionRequest(ResolveUserActionRequestParams {
            user_action_request_id: request.id.clone(),
            note: Some(truncate(&note, user_action_limits::MAX_NOTE_CHARS)),
            operation_id: derive_id(&format!("artq-resolve\0{}", request.id), "artq"),
        }))
    }

    fn apply(&self, mutation: WorkOutlineMutation) -> bool {
        let actor = self.outline.system_actor();
        match self.outline.mutate_as(&mutation, &actor) {
            Ok(_) => true,
            Err(error) => {
                tracing::warn!(code = error.code.as_str(), "artifact question sync: write failed");
                false
            }
        }
    }
}

/// Resolutions older than the artifact itself cannot answer its questions.
fn first_revision_at(artifact: &ArtifactView) -> i64 {
    artifact.created_at
}
