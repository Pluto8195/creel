//! Work Outline instruction delivery: what an adapter declares it can do
//! (`m0-instruction-delivery.md` section 6, contract 5.4-5.5).
//!
//! Every adapter is `Unsupported` until it opts in explicitly
//! ([`crate::Harness::instruction_support`]). A run whose request carries
//! `instructions` on an adapter that is not `Supported` must fail with
//! [`HarnessError::Capability`] before spawning, never run without them
//! ([`require_instruction_support`]).

use zeron_proto::{HarnessId, RunRequest};

use crate::HarnessError;

/// Where an adapter places the rendered envelope (drives the Effective
/// Instructions UI copy).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstructionChannel {
    /// Appended to the system prompt (Claude Code
    /// `--append-system-prompt-file`).
    SystemPromptAppend,
    /// A developer-instructions field of the session (Codex
    /// `developerInstructions`).
    DeveloperInstructions,
    /// The mock harness: recorded, not delivered to a model.
    Test,
}

/// How an adapter realizes `InstructionTransition::Replace` at a Turn
/// boundary. Always a new Execution Session; the variant says whether the
/// native conversation survives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstructionReplaceStrategy {
    /// Respawn and natively resume with the new envelope (conversation kept).
    RespawnResume,
    /// Resume with the new envelope, then compact so the old instructions
    /// leave the context before the next Turn (conversation summarized).
    CompactSwap,
    /// Start a fresh native session (conversation reset; the user confirms
    /// first, contract 5.7).
    FreshSession,
}

/// An adapter's declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstructionSupport {
    Unsupported {
        /// Why, in user-facing words.
        reason: String,
        /// The upgrade path, when one exists ("Update Claude Code to …").
        upgrade_hint: Option<String>,
    },
    Supported {
        channel: InstructionChannel,
        replace_strategy: InstructionReplaceStrategy,
        /// Largest rendered envelope the channel accepts, in UTF-8 bytes.
        max_bytes: usize,
    },
}

impl InstructionSupport {
    /// The default declaration of every adapter that has not opted in.
    pub fn no_channel() -> Self {
        Self::Unsupported {
            reason: "this harness has no channel for Work Outline instructions".into(),
            upgrade_hint: None,
        }
    }

    pub fn is_supported(&self) -> bool {
        matches!(self, Self::Supported { .. })
    }
}

/// Defense in depth for adapters' `run()`: a request that carries
/// instructions passes only when `support` is `Supported` and the rendered
/// envelope fits the channel. A request without instructions always passes.
pub fn require_instruction_support(
    harness: HarnessId,
    support: &InstructionSupport,
    request: &RunRequest,
) -> Result<(), HarnessError> {
    let Some(delivery) = &request.instructions else {
        return Ok(());
    };
    match support {
        InstructionSupport::Unsupported {
            reason,
            upgrade_hint,
        } => Err(HarnessError::Capability {
            harness,
            detail: reason.clone(),
            upgrade_hint: upgrade_hint.clone(),
        }),
        InstructionSupport::Supported { max_bytes, .. } => {
            let bytes = delivery.envelope.rendered.len();
            if bytes > *max_bytes {
                Err(HarnessError::Capability {
                    harness,
                    detail: format!(
                        "the instructions are {bytes} bytes; this harness accepts at most \
                         {max_bytes}"
                    ),
                    upgrade_hint: None,
                })
            } else {
                Ok(())
            }
        }
    }
}

/// Run `<exe> <args>` with stdin closed and return its stdout, killing it
/// after `timeout`. Used by the cached instruction-capability probes
/// (`claude --help`, `codex --version`), which are synchronous because
/// [`crate::Harness::instruction_support`] is.
pub(crate) fn bounded_output(
    exe: &std::path::Path,
    args: &[&str],
    timeout: std::time::Duration,
) -> std::io::Result<String> {
    use std::io::Read;
    let mut cmd = std::process::Command::new(exe);
    crate::compose_path(&mut cmd, exe.parent().filter(|d| !d.as_os_str().is_empty()));
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let mut child = cmd.spawn()?;
    let mut stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut out = String::new();
        if let Some(stdout) = stdout.as_mut() {
            let _ = stdout.read_to_string(&mut out);
        }
        out
    });
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("{} {} timed out", exe.display(), args.join(" ")),
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    reader
        .join()
        .map_err(|_| std::io::Error::other("probe output reader panicked"))
}

/// Per-executable-identity cache for instruction-capability probes: a CLI
/// upgraded or downgraded in place changes its modification time or size and
/// is probed again.
#[derive(Default)]
pub(crate) struct ProbeCache {
    slot: std::sync::Mutex<Option<(ProbeKey, InstructionSupport)>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProbeKey {
    path: std::path::PathBuf,
    modified: Option<std::time::SystemTime>,
    len: u64,
}

impl ProbeCache {
    pub(crate) fn get(
        &self,
        exe: &std::path::Path,
        probe: impl FnOnce(&std::path::Path) -> InstructionSupport,
    ) -> InstructionSupport {
        let meta = std::fs::metadata(exe).ok();
        let key = ProbeKey {
            path: exe.to_path_buf(),
            modified: meta.as_ref().and_then(|m| m.modified().ok()),
            len: meta.map(|m| m.len()).unwrap_or(0),
        };
        let mut slot = self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((cached, support)) = slot.as_ref()
            && *cached == key
        {
            return support.clone();
        }
        let support = probe(exe);
        *slot = Some((key, support.clone()));
        support
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeron_proto::{InstructionDelivery, InstructionEnvelope, InstructionTransition};

    fn request(rendered: Option<&str>) -> RunRequest {
        let mut request: RunRequest = serde_json::from_value(serde_json::json!({
            "prompt": "p", "model": null, "reasoning": null, "cwd": ".",
            "sandbox": "workspace-write", "resume": null
        }))
        .unwrap();
        request.instructions = rendered.map(|rendered| InstructionDelivery {
            envelope: InstructionEnvelope {
                revision_id: "r".into(),
                revision_hash: "h".into(),
                envelope_version: 1,
                envelope_sha256: "e".into(),
                rendered: rendered.into(),
            },
            transition: InstructionTransition::Fresh,
        });
        request
    }

    #[test]
    fn requests_without_instructions_always_pass() {
        assert!(
            require_instruction_support(
                HarnessId::Cursor,
                &InstructionSupport::no_channel(),
                &request(None)
            )
            .is_ok()
        );
    }

    #[test]
    fn unsupported_adapters_reject_instructions_with_the_upgrade_hint() {
        let support = InstructionSupport::Unsupported {
            reason: "Claude Code is too old".into(),
            upgrade_hint: Some("Update Claude Code".into()),
        };
        let error =
            require_instruction_support(HarnessId::ClaudeCode, &support, &request(Some("x")))
                .unwrap_err();
        match &error {
            HarnessError::Capability {
                harness,
                upgrade_hint,
                ..
            } => {
                assert_eq!(*harness, HarnessId::ClaudeCode);
                assert_eq!(upgrade_hint.as_deref(), Some("Update Claude Code"));
            }
            other => panic!("expected Capability, got {other:?}"),
        }
        let message = error.to_string();
        assert!(message.contains("cannot deliver Work Outline instructions"));
        assert!(message.contains("Claude Code is too old"));
    }

    #[test]
    fn supported_adapters_enforce_the_channel_size() {
        let support = InstructionSupport::Supported {
            channel: InstructionChannel::Test,
            replace_strategy: InstructionReplaceStrategy::RespawnResume,
            max_bytes: 4,
        };
        assert!(support.is_supported());
        assert!(
            require_instruction_support(HarnessId::Mock, &support, &request(Some("abcd"))).is_ok()
        );
        assert!(matches!(
            require_instruction_support(HarnessId::Mock, &support, &request(Some("abcde"))),
            Err(HarnessError::Capability { .. })
        ));
    }

    #[test]
    fn adapters_default_to_unsupported() {
        let mock = crate::mock::MockHarness { script: Vec::new() };
        assert!(!crate::Harness::instruction_support(&mock).is_supported());
    }
}

#[cfg(test)]
mod adapter_matrix_tests {
    use super::*;
    use crate::Harness;

    fn instruction_request() -> RunRequest {
        let mut request: RunRequest = serde_json::from_value(serde_json::json!({
            "prompt": "p", "model": null, "reasoning": null, "cwd": "/nonexistent-zeron-cwd",
            "sandbox": "workspace-write", "resume": null
        }))
        .unwrap();
        request.instructions = Some(zeron_proto::InstructionDelivery {
            envelope: zeron_proto::InstructionEnvelope {
                revision_id: "r".into(),
                revision_hash: "h".into(),
                envelope_version: 1,
                envelope_sha256: "e".into(),
                rendered: "x".into(),
            },
            transition: zeron_proto::InstructionTransition::Fresh,
        });
        request
    }

    fn controls() -> crate::RunControls {
        crate::RunControls {
            request_input: Box::new(|_| tokio::sync::oneshot::channel().1),
            steering: tokio::sync::mpsc::channel(1).1,
            interrupt: crate::CancellationToken::new(),
        }
    }

    /// Harnesses with no instruction channel refuse an instruction-bearing
    /// run with `Capability` before spawning anything (acceptance 12): the
    /// executables below do not exist, so reaching a spawn would surface
    /// `NotInstalled` instead.
    #[tokio::test]
    async fn adapters_without_a_channel_refuse_before_spawning() {
        let missing = "/nonexistent/zeron-harness-binary";
        let harnesses: Vec<Box<dyn Harness>> = vec![
            Box::new(crate::CursorHarness::new().with_executable(missing)),
            Box::new(crate::OpencodeHarness::new().with_executable(missing)),
            Box::new(crate::acp::AcpHarness::devin().with_executable(missing)),
        ];
        for harness in harnesses {
            assert!(
                !harness.instruction_support().is_supported(),
                "{:?}",
                harness.id()
            );
            let error = harness.run(instruction_request(), controls()).await.err();
            assert!(
                matches!(error, Some(HarnessError::Capability { .. })),
                "{:?}: {error:?}",
                harness.id()
            );
        }
    }
}
