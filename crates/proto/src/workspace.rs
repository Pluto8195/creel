//! Workspace lifecycle types shared by the engine and its clients.

use serde::{Deserialize, Serialize};

/// Protocol features are advertised explicitly because personal/integration
/// builds may share a semver with upstream while exposing a different RPC and
/// document surface.
pub mod capabilities {
    pub const MESSAGE_QUEUE_V1: &str = "message-queue-v1";
    pub const MESSAGE_QUEUE_ACTIONS_V1: &str = "message-queue-actions-v1";
    pub const MESSAGE_QUEUE_ATTACHMENTS_V1: &str = "message-queue-attachments-v1";
    pub const MESSAGE_QUEUE_CLEAN_ATTACHMENT_TEXT_V1: &str =
        "message-queue-clean-attachment-text-v1";
    pub const MESSAGE_QUEUE_EDIT_LEASE_V1: &str = "message-queue-edit-lease-v1";
    /// Work Outline rows are readable and writable on this host (registry
    /// rows, `WatchWorkOutline`, `WorkOutlineMutate`).
    pub const WORK_OUTLINE_V1: &str = "work-outline-v1";
    /// The host understands instruction-bearing commands (`runWithInstructions`)
    /// and queued-row pins, resolves, verifies, and delivers Work Outline
    /// instruction envelopes (version 1), and rejects rather than omits them.
    /// Hosts also stamp one [`work_outline_instruction_envelope_for`] token per
    /// harness whose delivery probe passes
    /// ([`current_with_instruction_harnesses`]).
    pub const WORK_OUTLINE_INSTRUCTION_ENVELOPE_V1: &str = "work-outline-instruction-envelope-v1";
    /// The host stores, resolves, and validates multi-repository execution
    /// configurations (`configureExecution`, `ValidateExecutionConfiguration`,
    /// `DescribeExecutionRepository`). Clients forward validation only to
    /// owners that advertise it.
    pub const WORK_OUTLINE_MULTI_REPO_V1: &str = "work-outline-multi-repo-v1";
    /// The host runs the idempotent Work Outline launch workflow
    /// (`CreateChatForWorkItem`, `ContinueChatForWorkItem`) and enforces the
    /// captured execution binding at every dispatch. Clients forward a launch
    /// only to owners that advertise it.
    pub const WORK_OUTLINE_LAUNCH_V1: &str = "work-outline-launch-v1";
    /// The host runs `StartPrReview` (check out a PR like the Cofactr
    /// launcher and start a review Chat). Clients forward a PR review start
    /// only to devices that advertise it.
    pub const PR_REVIEW_LAUNCH_V1: &str = "pr-review-launch-v1";
    /// The host serves the Linear RPCs (`LinearStatus` …) and understands the
    /// Linear Work Outline operations (`setSpaceLinearEnabled`,
    /// `createWorkItemFromLinear`, `linkLinearProject`).
    pub const LINEAR_V1: &str = crate::linear::LINEAR_CAPABILITY_V1;
    /// The host runs the PR review sync for its Spaces, serves the `PR_*`
    /// RPCs (`PrStatus`, `PrReviewSyncNow`, `PrMyOpen`, `PrReviewRequested`),
    /// and understands `setPrReviewSyncDestination` and system-origin
    /// Required Actions.
    pub const PR_REVIEW_SYNC_V1: &str = crate::pr_review_sync::PR_REVIEW_SYNC_CAPABILITY_V1;

    /// Per-harness token: `work-outline-instruction-envelope-v1/<harness>`,
    /// where `<harness>` is the harness id's wire spelling (`claude-code`,
    /// `codex`, ...).
    pub fn work_outline_instruction_envelope_for(harness: crate::HarnessId) -> String {
        let name = serde_json::to_value(harness)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_default();
        format!("{WORK_OUTLINE_INSTRUCTION_ENVELOPE_V1}/{name}")
    }

    pub const CURRENT: &[&str] = &[
        MESSAGE_QUEUE_V1,
        MESSAGE_QUEUE_ACTIONS_V1,
        MESSAGE_QUEUE_ATTACHMENTS_V1,
        MESSAGE_QUEUE_CLEAN_ATTACHMENT_TEXT_V1,
        MESSAGE_QUEUE_EDIT_LEASE_V1,
        WORK_OUTLINE_V1,
        WORK_OUTLINE_INSTRUCTION_ENVELOPE_V1,
        WORK_OUTLINE_MULTI_REPO_V1,
        WORK_OUTLINE_LAUNCH_V1,
        PR_REVIEW_LAUNCH_V1,
        LINEAR_V1,
        PR_REVIEW_SYNC_V1,
    ];

    pub fn current() -> Vec<String> {
        CURRENT.iter().map(|value| (*value).to_string()).collect()
    }

    /// [`current`] plus the per-harness instruction tokens for `harnesses`
    /// (those whose delivery probe passed on this host).
    pub fn current_with_instruction_harnesses(harnesses: &[crate::HarnessId]) -> Vec<String> {
        let mut all = current();
        for harness in harnesses {
            let token = work_outline_instruction_envelope_for(*harness);
            if !all.contains(&token) {
                all.push(token);
            }
        }
        all
    }

    /// Whether `token` is a per-harness instruction token.
    pub fn is_instruction_harness_token(token: &str) -> bool {
        token
            .strip_prefix(WORK_OUTLINE_INSTRUCTION_ENVELOPE_V1)
            .is_some_and(|rest| rest.starts_with('/') && rest.len() > 1)
    }

    /// [`current`] plus the per-harness instruction tokens a previous stamp
    /// advertised: boot re-registration keeps them until the probes restamp,
    /// so a restart never briefly reports "no harness can deliver".
    pub fn current_preserving_instruction_harnesses(previous: &[String]) -> Vec<String> {
        let mut all = current();
        for token in previous {
            if is_instruction_harness_token(token) && !all.contains(token) {
                all.push(token.clone());
            }
        }
        all
    }
}

/// The fixed data boundary selected when an engine runtime is assembled.
///
/// Authentication can change while a runtime is alive, but its workspace scope
/// cannot. Switching scopes requires assembling a new runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceScope {
    Local,
    Synced,
    Development,
}

/// Stable information about the engine runtime reached by a client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineInfo {
    pub device_id: String,
    pub workspace_scope: WorkspaceScope,
    /// SDK selected by the owning engine, absent on older versions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor_sdk_version: Option<String>,
    /// Supported protocol/document features. Missing on older engines.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
}

impl EngineInfo {
    pub fn supports(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|value| value == capability)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_scope_uses_wire_safe_names() {
        for (scope, encoded) in [
            (WorkspaceScope::Local, "\"local\""),
            (WorkspaceScope::Synced, "\"synced\""),
            (WorkspaceScope::Development, "\"development\""),
        ] {
            assert_eq!(serde_json::to_string(&scope).unwrap(), encoded);
            assert_eq!(
                serde_json::from_str::<WorkspaceScope>(encoded).unwrap(),
                scope
            );
        }
    }

    #[test]
    fn engine_info_uses_camel_case_fields() {
        let info = EngineInfo {
            device_id: "device-1".into(),
            workspace_scope: WorkspaceScope::Local,
            cursor_sdk_version: Some("1.0.31".into()),
            capabilities: capabilities::current(),
        };
        assert_eq!(
            serde_json::to_value(&info).unwrap(),
            serde_json::json!({
                "deviceId": "device-1",
                "workspaceScope": "local",
                "cursorSdkVersion": "1.0.31",
                "capabilities": [
                    "message-queue-v1",
                    "message-queue-actions-v1",
                    "message-queue-attachments-v1",
                    "message-queue-clean-attachment-text-v1",
                    "message-queue-edit-lease-v1",
                    "work-outline-v1",
                    "work-outline-instruction-envelope-v1",
                    "work-outline-multi-repo-v1",
                    "work-outline-launch-v1",
                    "pr-review-launch-v1",
                    "linear-v1",
                    "pr-review-sync-v1"
                ],
            })
        );
    }

    #[test]
    fn boot_keeps_previously_stamped_instruction_harness_tokens() {
        let previous = vec![
            "work-outline-instruction-envelope-v1/codex".to_string(),
            "work-outline-instruction-envelope-v1".to_string(),
            "work-outline-instruction-envelope-v1/".to_string(),
            "retired-capability".to_string(),
        ];
        let stamped = capabilities::current_preserving_instruction_harnesses(&previous);
        let mut expected = capabilities::current();
        expected.push("work-outline-instruction-envelope-v1/codex".into());
        assert_eq!(stamped, expected);
        assert_eq!(
            capabilities::current_preserving_instruction_harnesses(&[]),
            capabilities::current()
        );
    }

    #[test]
    fn instruction_envelope_capability_tokens_are_pinned() {
        assert_eq!(
            capabilities::WORK_OUTLINE_INSTRUCTION_ENVELOPE_V1,
            "work-outline-instruction-envelope-v1"
        );
        assert_eq!(
            capabilities::work_outline_instruction_envelope_for(crate::HarnessId::ClaudeCode),
            "work-outline-instruction-envelope-v1/claude-code"
        );
        assert_eq!(
            capabilities::work_outline_instruction_envelope_for(crate::HarnessId::Codex),
            "work-outline-instruction-envelope-v1/codex"
        );
        // Delivery shipped: the engine token is static; harness tokens are
        // added per probe.
        assert!(
            capabilities::CURRENT.contains(&capabilities::WORK_OUTLINE_INSTRUCTION_ENVELOPE_V1)
        );
        let probed = capabilities::current_with_instruction_harnesses(&[
            crate::HarnessId::ClaudeCode,
            crate::HarnessId::ClaudeCode,
        ]);
        assert_eq!(
            probed
                .iter()
                .filter(|t| t.as_str() == "work-outline-instruction-envelope-v1/claude-code")
                .count(),
            1
        );
        assert!(!probed.iter().any(|t| t.ends_with("/codex")));
    }

    #[test]
    fn old_engine_info_defaults_to_no_capabilities() {
        let info: EngineInfo = serde_json::from_value(serde_json::json!({
            "deviceId": "old",
            "workspaceScope": "synced"
        }))
        .unwrap();
        assert!(info.capabilities.is_empty());
        assert!(info.cursor_sdk_version.is_none());
        assert!(!info.supports(capabilities::MESSAGE_QUEUE_V1));
    }
}
