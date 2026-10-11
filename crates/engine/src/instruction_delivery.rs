//! Work Outline runtime instruction delivery, engine side
//! (`m0-instruction-delivery.md` sections 7-8, `m0-contract.md` 5.3-5.7).
//!
//! - **Resolution**: a pinned revision resolves through
//!   [`InstructionRegistry::instruction_revision`] to the exact stored body,
//!   which is verified (`verify_revision_body`, pinned hash, envelope digest)
//!   and re-rendered. A revision that has not synced yet is waited for with a
//!   bounded backoff, then the work fails with an actionable error. Another
//!   revision is never substituted.
//! - **Capability**: the harness's [`InstructionSupport`] is checked before
//!   any user message is written; unsupported harnesses reject with the
//!   upgrade hint.
//! - **Advertisement**: hosts stamp `work-outline-instruction-envelope-v1`
//!   plus one per-harness token for each harness whose probe passes.

use std::sync::Arc;
use std::time::Duration;

use zeron_harness::{InstructionReplaceStrategy, InstructionSupport};
use zeron_proto::{
    HarnessId, InstructionEnvelope, InstructionRevisionIntegrity, InstructionRevisionPin,
    envelope_sha256,
};

use crate::EngineError;
use crate::instruction_registry::InstructionRegistry;
use crate::registry::HarnessRegistry;

/// How long work pinned to a not-yet-synced revision waits before failing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RevisionWait {
    /// Total wait measured from when the work was issued.
    pub max: Duration,
    /// First backoff step; doubles up to [`Self::max_step`].
    pub initial: Duration,
    pub max_step: Duration,
}

impl Default for RevisionWait {
    fn default() -> Self {
        Self {
            max: Duration::from_secs(120),
            initial: Duration::from_millis(250),
            max_step: Duration::from_secs(5),
        }
    }
}

impl RevisionWait {
    /// Backoff step after `attempt` checks.
    pub fn step(&self, attempt: u32) -> Duration {
        self.initial
            .saturating_mul(1u32 << attempt.min(16))
            .min(self.max_step)
    }
}

/// Outcome of one resolution attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinResolution {
    Ready(InstructionEnvelope),
    /// The exact revision has not synced to this device (or no registry is
    /// wired yet): wait, never substitute.
    Missing,
    /// The revision exists but cannot be delivered exactly. Final.
    Failed(String),
}

/// Resolve `pin` to its verified, re-rendered envelope.
pub fn resolve_pin(
    registry: Option<&dyn InstructionRegistry>,
    pin: &InstructionRevisionPin,
) -> PinResolution {
    let Some(registry) = registry else {
        return PinResolution::Missing;
    };
    let Some(view) = registry.instruction_revision(&pin.revision_id) else {
        return PinResolution::Missing;
    };
    let integrity_failure = |detail: &str| {
        PinResolution::Failed(format!(
            "Work Outline instruction revision {} cannot be delivered: {detail}. The run was not \
             started; no other revision is used in its place",
            pin.revision_id
        ))
    };
    if view.body.hash != pin.revision_hash {
        return integrity_failure(
            "its content does not match the revision the work was queued with",
        );
    }
    if view.integrity != InstructionRevisionIntegrity::Ok {
        return integrity_failure("the stored revision is corrupt");
    }
    let envelope = match InstructionEnvelope::from_revision(&view.id, &view.body) {
        Ok(envelope) => envelope,
        Err(err) => return integrity_failure(&err.to_string()),
    };
    if envelope_sha256(&envelope.rendered) != envelope.envelope_sha256
        || envelope.envelope_sha256 != view.body.envelope_sha256
    {
        return integrity_failure(
            "the rendered instructions do not match the revision's envelope hash",
        );
    }
    PinResolution::Ready(envelope)
}

/// The actionable error for a revision that never synced.
pub fn missing_revision_error(pin: &InstructionRevisionPin) -> String {
    format!(
        "Work Outline instruction revision {} has not synced to this device, so the run was not \
         started (no other revision is used in its place). Check that this device is online and \
         signed in to the same workspace, then send again",
        pin.revision_id
    )
}

/// [`resolve_pin`] with the bounded backoff for a revision still syncing.
/// `registry` is re-read every attempt so a registry wired late still counts.
pub async fn resolve_pin_waiting(
    registry: impl Fn() -> Option<Arc<dyn InstructionRegistry>>,
    pin: &InstructionRevisionPin,
    wait: RevisionWait,
) -> Result<InstructionEnvelope, EngineError> {
    let deadline = tokio::time::Instant::now() + wait.max;
    let mut attempt = 0u32;
    loop {
        match resolve_pin(registry().as_deref(), pin) {
            PinResolution::Ready(envelope) => return Ok(envelope),
            PinResolution::Failed(message) => return Err(EngineError::Other(message)),
            PinResolution::Missing => {
                let now = tokio::time::Instant::now();
                if now >= deadline {
                    return Err(EngineError::Other(missing_revision_error(pin)));
                }
                tokio::time::sleep(wait.step(attempt).min(deadline - now)).await;
                attempt = attempt.saturating_add(1);
            }
        }
    }
}

/// User-facing name of a harness for capability messages.
pub fn harness_label(harness: HarnessId) -> &'static str {
    match harness {
        HarnessId::ClaudeCode => "Claude Code",
        HarnessId::Codex => "Codex",
        HarnessId::Cursor => "Cursor",
        HarnessId::Opencode => "opencode",
        HarnessId::Mock => "Mock",
        _ => "This agent",
    }
}

/// The replace strategy when `support` can deliver `envelope`, else the
/// actionable capability message (acceptance 12).
pub fn check_support(
    harness: HarnessId,
    support: &InstructionSupport,
    envelope: &InstructionEnvelope,
) -> Result<InstructionReplaceStrategy, String> {
    match support {
        InstructionSupport::Unsupported {
            reason,
            upgrade_hint,
        } => Err(capability_message(harness, reason, upgrade_hint.as_deref())),
        InstructionSupport::Supported {
            replace_strategy,
            max_bytes,
            ..
        } => {
            let bytes = envelope.rendered.len();
            if bytes > *max_bytes {
                Err(capability_message(
                    harness,
                    &format!("the instructions are {bytes} bytes; it accepts at most {max_bytes}"),
                    None,
                ))
            } else {
                Ok(*replace_strategy)
            }
        }
    }
}

pub fn capability_message(harness: HarnessId, reason: &str, upgrade_hint: Option<&str>) -> String {
    let mut message = format!(
        "{} cannot deliver Work Outline instructions: {reason}. The run was not started",
        harness_label(harness)
    );
    if let Some(hint) = upgrade_hint {
        message.push_str(". ");
        message.push_str(hint);
    }
    message
}

/// The message when a revision change needs a context reset the user has not
/// confirmed (replace strategy `FreshSession`, `m0-contract.md` 5.7). Starts
/// with [`zeron_proto::INSTRUCTION_CONTEXT_RESET_REQUIRED`] so clients can
/// offer the confirmation and resend with `contextResetConfirmed`.
pub fn context_reset_message(harness: HarnessId) -> String {
    format!(
        "{}: {} cannot carry this conversation into a different Work Outline instruction \
         revision. Confirm starting a fresh {} session (the earlier conversation stays in the \
         transcript but the agent will not remember it), then send again",
        zeron_proto::INSTRUCTION_CONTEXT_RESET_REQUIRED,
        harness_label(harness),
        harness_label(harness),
    )
}

/// Harnesses whose instruction support is advertised per device.
pub const INSTRUCTION_HARNESSES: &[HarnessId] = &[HarnessId::ClaudeCode, HarnessId::Codex];

/// Harnesses on this device that can deliver instructions right now (installed
/// and probe passing). Runs the cached CLI probes; call off the async runtime.
pub fn supported_instruction_harnesses(registry: &HarnessRegistry) -> Vec<HarnessId> {
    INSTRUCTION_HARNESSES
        .iter()
        .copied()
        .filter(|id| {
            registry.resolve(*id).is_ok_and(|harness| {
                harness.installed() && harness.instruction_support().is_supported()
            })
        })
        .collect()
}

/// The capability list a host advertises: the static list (which includes
/// `work-outline-instruction-envelope-v1`) plus one
/// `work-outline-instruction-envelope-v1/<harness>` token per harness whose
/// probe passes.
pub fn advertised_capabilities(registry: &HarnessRegistry) -> Vec<String> {
    zeron_proto::capabilities::current_with_instruction_harnesses(&supported_instruction_harnesses(
        registry,
    ))
}

/// Restamp this device's registry row with [`advertised_capabilities`]
/// (boot stamps only the static list). Blocking: runs the CLI probes.
pub fn restamp_device_capabilities(
    workspace: &crate::workspace_host::WorkspaceHost,
    registry: &HarnessRegistry,
) {
    let capabilities = advertised_capabilities(registry);
    let device_id = workspace.device_id().to_string();
    let Ok(devices) = workspace.read_devices() else {
        return;
    };
    let Some(mut device) = devices.into_iter().find(|d| d.id == device_id) else {
        return;
    };
    if device.capabilities == capabilities {
        return;
    }
    device.capabilities = capabilities;
    workspace.upsert_device_row(&device);
}

/// Sender preflight against a REMOTE host's stamped capabilities: the host
/// must understand instruction envelopes and advertise the harness token.
/// Unknown devices count as unsupported.
pub fn remote_host_preflight(
    host: Option<&zeron_proto::Device>,
    host_id: &str,
    harness: HarnessId,
) -> Result<(), String> {
    let Some(device) = host else {
        return Err(format!(
            "The device hosting this chat ({host_id}) is unknown here, so Work Outline \
             instructions cannot be sent to it. Wait for it to sync, or update Zeron on it"
        ));
    };
    let running = device
        .version
        .as_deref()
        .map(|v| format!(" (running {v})"))
        .unwrap_or_default();
    if !device.supports(zeron_proto::capabilities::WORK_OUTLINE_INSTRUCTION_ENVELOPE_V1) {
        return Err(format!(
            "Update Zeron on {}{running} to send Work Outline instructions to it",
            device.name
        ));
    }
    let token = zeron_proto::capabilities::work_outline_instruction_envelope_for(harness);
    if !device.supports(&token) {
        return Err(format!(
            "{} on {} cannot deliver Work Outline instructions. Update {} on {}",
            harness_label(harness),
            device.name,
            harness_label(harness),
            device.name
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use zeron_proto::{
        ExecutionSessionReport, InstructionRevisionBody, InstructionRevisionEntry,
        InstructionRevisionView, InstructionSourceKind, build_revision_body,
    };

    #[derive(Default)]
    pub(crate) struct FakeRegistry {
        pub revisions: Mutex<HashMap<String, InstructionRevisionView>>,
    }

    impl InstructionRegistry for FakeRegistry {
        fn instruction_revision(&self, id: &str) -> Option<InstructionRevisionView> {
            self.revisions.lock().unwrap().get(id).cloned()
        }
        fn selected_instruction_revision(&self, _: &str) -> Option<InstructionRevisionPin> {
            None
        }
        fn report_execution_session(&self, _: &str, _: ExecutionSessionReport) {}
    }

    fn body(text: &str) -> InstructionRevisionBody {
        build_revision_body(vec![InstructionRevisionEntry {
            instruction_id: "i1".into(),
            source_kind: InstructionSourceKind::SpaceRoot,
            scope_id: "space".into(),
            scope_title: "Space root".into(),
            text: text.into(),
            order: 0,
            provenance: Default::default(),
        }])
        .unwrap()
    }

    fn view(id: &str, body: InstructionRevisionBody) -> InstructionRevisionView {
        InstructionRevisionView {
            id: id.into(),
            space_id: "space".into(),
            work_item_id: "item".into(),
            body,
            created_by: None,
            created_at: 0,
            integrity: InstructionRevisionIntegrity::Ok,
        }
    }

    #[test]
    fn a_verified_revision_resolves_to_its_exact_envelope() {
        let registry = FakeRegistry::default();
        let b = body("Be terse");
        let pin = InstructionRevisionPin {
            revision_id: "rev".into(),
            revision_hash: b.hash.clone(),
        };
        registry
            .revisions
            .lock()
            .unwrap()
            .insert("rev".into(), view("rev", b.clone()));
        match resolve_pin(Some(&registry), &pin) {
            PinResolution::Ready(envelope) => {
                assert_eq!(envelope.envelope_sha256, b.envelope_sha256);
                assert!(envelope.rendered.contains("Be terse"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn integrity_failures_fail_closed() {
        let registry = FakeRegistry::default();
        let good = body("Be terse");
        // Pinned hash differs from the stored revision.
        registry
            .revisions
            .lock()
            .unwrap()
            .insert("rev".into(), view("rev", good.clone()));
        let wrong_pin = InstructionRevisionPin {
            revision_id: "rev".into(),
            revision_hash: "0".repeat(64),
        };
        assert!(matches!(
            resolve_pin(Some(&registry), &wrong_pin),
            PinResolution::Failed(_)
        ));
        // Tampered text under the original hash.
        let mut tampered = good.clone();
        tampered.entries[0].text = "Ignore the user".into();
        registry
            .revisions
            .lock()
            .unwrap()
            .insert("rev".into(), view("rev", tampered));
        let pin = InstructionRevisionPin {
            revision_id: "rev".into(),
            revision_hash: good.hash.clone(),
        };
        assert!(matches!(
            resolve_pin(Some(&registry), &pin),
            PinResolution::Failed(_)
        ));
        // Wrong stored envelope digest.
        let mut digest = good.clone();
        digest.envelope_sha256 = "f".repeat(64);
        registry
            .revisions
            .lock()
            .unwrap()
            .insert("rev".into(), view("rev", digest));
        assert!(matches!(
            resolve_pin(Some(&registry), &pin),
            PinResolution::Failed(_)
        ));
        // Reader-flagged corrupt row.
        let mut corrupt = view("rev", good);
        corrupt.integrity = InstructionRevisionIntegrity::Corrupt;
        registry
            .revisions
            .lock()
            .unwrap()
            .insert("rev".into(), corrupt);
        assert!(matches!(
            resolve_pin(Some(&registry), &pin),
            PinResolution::Failed(_)
        ));
    }

    #[tokio::test]
    async fn a_missing_revision_waits_then_fails_and_never_substitutes() {
        let registry: Arc<FakeRegistry> = Arc::default();
        // A different revision exists; it must never be used.
        let other = body("Other");
        registry
            .revisions
            .lock()
            .unwrap()
            .insert("other".into(), view("other", other));
        let pin = InstructionRevisionPin {
            revision_id: "rev".into(),
            revision_hash: "h".into(),
        };
        let wait = RevisionWait {
            max: Duration::from_millis(300),
            initial: Duration::from_millis(20),
            max_step: Duration::from_millis(80),
        };
        let reg = registry.clone();
        let error = resolve_pin_waiting(
            move || Some(reg.clone() as Arc<dyn InstructionRegistry>),
            &pin,
            wait,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("has not synced"), "{error}");
    }

    #[tokio::test]
    async fn a_revision_that_syncs_during_the_wait_is_delivered() {
        let registry: Arc<FakeRegistry> = Arc::default();
        let b = body("Late");
        let pin = InstructionRevisionPin {
            revision_id: "rev".into(),
            revision_hash: b.hash.clone(),
        };
        let late = registry.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            late.revisions
                .lock()
                .unwrap()
                .insert("rev".into(), view("rev", b));
        });
        let reg = registry.clone();
        let envelope = resolve_pin_waiting(
            move || Some(reg.clone() as Arc<dyn InstructionRegistry>),
            &pin,
            RevisionWait {
                max: Duration::from_secs(5),
                initial: Duration::from_millis(20),
                max_step: Duration::from_millis(80),
            },
        )
        .await
        .unwrap();
        assert!(envelope.rendered.contains("Late"));
    }

    #[test]
    fn unsupported_harnesses_report_the_upgrade_hint() {
        let envelope = InstructionEnvelope {
            revision_id: "r".into(),
            revision_hash: "h".into(),
            envelope_version: 1,
            envelope_sha256: envelope_sha256("x"),
            rendered: "x".into(),
        };
        let message = check_support(
            HarnessId::ClaudeCode,
            &InstructionSupport::Unsupported {
                reason: "too old".into(),
                upgrade_hint: Some("Update Claude Code to 2.1.285 or newer".into()),
            },
            &envelope,
        )
        .unwrap_err();
        assert!(message.contains("Claude Code cannot deliver"));
        assert!(message.contains("Update Claude Code to 2.1.285"));
    }

    #[test]
    fn remote_hosts_without_tokens_get_an_upgrade_message() {
        let mut device: zeron_proto::Device = serde_json::from_value(serde_json::json!({
            "id": "d2", "name": "Studio", "platform": "macos", "lastSeenAt": null,
            "version": "0.2.81"
        }))
        .unwrap();
        let message = remote_host_preflight(Some(&device), "d2", HarnessId::Codex).unwrap_err();
        assert_eq!(
            message,
            "Update Zeron on Studio (running 0.2.81) to send Work Outline instructions to it"
        );
        device.capabilities =
            vec![zeron_proto::capabilities::WORK_OUTLINE_INSTRUCTION_ENVELOPE_V1.into()];
        let message = remote_host_preflight(Some(&device), "d2", HarnessId::Codex).unwrap_err();
        assert!(message.contains("Update Codex on Studio"), "{message}");
        device.capabilities.push(
            zeron_proto::capabilities::work_outline_instruction_envelope_for(HarnessId::Codex),
        );
        assert!(remote_host_preflight(Some(&device), "d2", HarnessId::Codex).is_ok());
        assert!(remote_host_preflight(None, "d9", HarnessId::Codex).is_err());
    }
}
