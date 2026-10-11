//! Work Outline launch (Milestone 3 stage 2): the wire types of the two
//! execution-touching operations, `CreateChatForWorkItem` and
//! `ContinueChatForWorkItem` (spec "Service operations"; contract section 4,
//! idempotency audit section 7).
//!
//! Both are dedicated RPC methods, not `WorkOutlineMutate` ops: a launch must
//! run on the Space's owning device (it validates paths, may create a
//! worktree, and queues the first Turn), while `WorkOutlineMutate` is a local
//! LWW registry write accepted anywhere. `CreateChatForWorkItem` is
//! forwardable (the receiving engine forwards it to the owner itself);
//! `ContinueChatForWorkItem` queues durably from any device and the host
//! revalidates the binding at drain (contract decision 6.4.1).
//!
//! Every record id is derived from the `operationId`
//! ([`crate::work_outline::derive_id`]): Chat `chat`, first user message
//! `message`, command `command`, revision `revision`, application
//! `application`. The execution binding and the assignment are keyed by the
//! Chat id. A retry with the same `operationId` and inputs completes the
//! missing steps and returns the same ids; other inputs are
//! `operationConflict`.

use serde::{Deserialize, Serialize};

use crate::entities::ChatConfig;
use crate::work_outline_execution::ExecutionConfiguration;

/// The prompt of a launch or continuation: the visible text plus image
/// attachments exactly as a `Run` request carries them (absolute paths, or
/// `pending://` refs already uploaded to the owning device).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkItemPrompt {
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<String>,
}

/// A deliberate per-launch replacement of the work item's resolved execution
/// configuration (the Chat then shows as an execution exception).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionOverride {
    pub configuration: ExecutionConfiguration,
}

/// Which checkout the launch runs in when the configuration's target
/// repository (the primary binding, else the only binding, else the
/// repository at the execution root) does not pin a worktree. Internally
/// tagged by `type`. Strict (request enum).
///
/// - `useExisting`: an existing registered worktree of that repository
///   (`worktreeId` = its `CheckoutIdentity.id`, as `DescribeExecutionRepository`
///   reports it).
/// - `createNew`: create `<superproject>/.worktrees/<label>/<slug(name)>` with
///   the existing local chat-worktree flow (`label` is derived by that flow:
///   `workspace`, or the member repository's folder name). Idempotent for the
///   operation: a retry reuses the worktree this operation created and fails
///   (never falls back to the base checkout) when something else is there.
///
/// When the execution root IS the target repository's checkout, the chosen
/// worktree also becomes the execution root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum WorktreeChoice {
    #[serde(rename_all = "camelCase")]
    UseExisting {
        worktree_id: String,
        worktree_path: String,
    },
    #[serde(rename_all = "camelCase")]
    CreateNew { name: String },
}

/// Params of `CreateChatForWorkItem`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateChatForWorkItemParams {
    pub work_item_id: String,
    pub initial_prompt: WorkItemPrompt,
    /// Harness, model, reasoning, options, sandbox, approval: the same
    /// `ChatConfig` the `createChat` mutation takes. Absent = the host's
    /// default harness with default options.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<ChatConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_override: Option<ExecutionOverride>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_choice: Option<WorktreeChoice>,
    pub operation_id: String,
    /// Device routing (the Space's owner). Optional: the receiving engine
    /// forwards to the owner on its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_device_id: Option<String>,
}

/// Params of `ContinueChatForWorkItem`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContinueChatForWorkItemParams {
    pub chat_id: String,
    pub prompt: WorkItemPrompt,
    pub operation_id: String,
}

/// Where the launched (or continued) Turn is, read from its command.
/// Lenient: an unknown value reads as `queued`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkItemLaunchState {
    /// The host executed the command (the Turn started or was held at a Turn
    /// boundary).
    Started,
    /// The host rejected the command; `detail` says why. A retry with the
    /// same `operationId` re-issues it once the cause is fixed.
    Rejected,
    /// The Turn is durably queued and waits for the host's drain. (Last so
    /// it can be the lenient fallback.)
    #[default]
    #[serde(other)]
    Queued,
}

/// Reply of `CreateChatForWorkItem`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateChatForWorkItemReply {
    pub chat_id: String,
    pub message_id: String,
    pub command_id: String,
    /// The launch revision and its application; absent when the item had no
    /// effective instructions (the first Turn is then a plain `Run`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub application_id: Option<String>,
    /// The execution root the Chat was created in (never retargeted).
    pub execution_root: String,
    pub state: WorkItemLaunchState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Reply of `ContinueChatForWorkItem`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContinueChatForWorkItemReply {
    pub chat_id: String,
    pub message_id: String,
    pub command_id: String,
    pub state: WorkItemLaunchState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HarnessId, SandboxLevel};
    use serde_json::json;

    #[test]
    fn create_params_wire_shape() {
        let params = CreateChatForWorkItemParams {
            work_item_id: "item".into(),
            initial_prompt: WorkItemPrompt {
                text: "Fix it".into(),
                attachments: vec!["/tmp/a.png".into()],
            },
            config: Some(ChatConfig {
                harness: HarnessId::Mock,
                model: Some("m".into()),
                reasoning: None,
                model_options: Default::default(),
                sandbox: SandboxLevel::WorkspaceWrite,
                auto_approve: true,
            }),
            execution_override: Some(ExecutionOverride {
                configuration: ExecutionConfiguration::implicit("/repo"),
            }),
            worktree_choice: Some(WorktreeChoice::CreateNew {
                name: "fix it".into(),
            }),
            operation_id: "op".into(),
            target_device_id: None,
        };
        let value = serde_json::to_value(&params).unwrap();
        assert_eq!(
            value,
            json!({
                "workItemId": "item",
                "initialPrompt": { "text": "Fix it", "attachments": ["/tmp/a.png"] },
                "config": {
                    "harness": "mock", "model": "m", "reasoning": null, "modelOptions": {},
                    "sandbox": "workspace-write", "autoApprove": true
                },
                "executionOverride": {
                    "configuration": { "executionRoot": "/repo", "bindings": [], "schema": 1 }
                },
                "worktreeChoice": { "type": "createNew", "name": "fix it" },
                "operationId": "op",
            })
        );
        let back: CreateChatForWorkItemParams = serde_json::from_value(value).unwrap();
        assert_eq!(back, params);
        let existing: WorktreeChoice = serde_json::from_value(json!({
            "type": "useExisting", "worktreeId": "w", "worktreePath": "/repo/.worktrees/x"
        }))
        .unwrap();
        assert_eq!(
            existing,
            WorktreeChoice::UseExisting {
                worktree_id: "w".into(),
                worktree_path: "/repo/.worktrees/x".into(),
            }
        );
        // Minimal form.
        let minimal: CreateChatForWorkItemParams = serde_json::from_value(json!({
            "workItemId": "item", "initialPrompt": { "text": "go" }, "operationId": "op"
        }))
        .unwrap();
        assert!(minimal.config.is_none() && minimal.worktree_choice.is_none());
    }

    #[test]
    fn replies_and_continue_wire_shape() {
        let reply = CreateChatForWorkItemReply {
            chat_id: "c".into(),
            message_id: "m".into(),
            command_id: "k".into(),
            revision_id: Some("r".into()),
            application_id: Some("a".into()),
            execution_root: "/repo".into(),
            state: WorkItemLaunchState::Queued,
            detail: None,
        };
        assert_eq!(
            serde_json::to_value(&reply).unwrap(),
            json!({
                "chatId": "c", "messageId": "m", "commandId": "k", "revisionId": "r",
                "applicationId": "a", "executionRoot": "/repo", "state": "queued"
            })
        );
        let cont = ContinueChatForWorkItemReply {
            chat_id: "c".into(),
            message_id: "m".into(),
            command_id: "k".into(),
            state: WorkItemLaunchState::Rejected,
            detail: Some("why".into()),
        };
        assert_eq!(
            serde_json::to_value(&cont).unwrap(),
            json!({ "chatId": "c", "messageId": "m", "commandId": "k", "state": "rejected",
                    "detail": "why" })
        );
        let params: ContinueChatForWorkItemParams = serde_json::from_value(json!({
            "chatId": "c", "prompt": { "text": "more" }, "operationId": "op"
        }))
        .unwrap();
        assert_eq!(params.prompt.text, "more");
        let state: WorkItemLaunchState = serde_json::from_value(json!("later")).unwrap();
        assert_eq!(state, WorkItemLaunchState::Queued);
        assert_eq!(
            serde_json::to_value(WorkItemLaunchState::Started).unwrap(),
            json!("started")
        );
    }
}
