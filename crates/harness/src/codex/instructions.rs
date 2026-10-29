//! Work Outline instruction delivery for Codex (`m0-instruction-delivery.md`
//! sections 2 and 5; Milestone 2 trials in the same note).
//!
//! The envelope rides `developerInstructions` on `thread/start`, and the
//! session's exact applied revision is re-supplied on every `thread/resume`:
//! resume ignores the field until the next compaction, which then rebuilds
//! context from whatever the resume supplied (X7, X8), so omitting it loses
//! the instructions and a different value swaps them silently.
//!
//! A revision change inside an existing thread is not offered. The candidate
//! "resume with the new value, then `thread/compact/start`" passed only 4 of 6
//! real trials (the compacted history still carried the old instructions in
//! two), so the replace strategy is `FreshSession`: the engine starts a new
//! thread after the user confirms the context reset, and this adapter refuses
//! a `Replace` aimed at an existing thread.

use std::path::Path;
use std::time::Duration;

use mast_proto::{HarnessId, InstructionTransition, RunRequest};

use crate::{HarnessError, InstructionChannel, InstructionReplaceStrategy, InstructionSupport};

/// The Codex CLI version the delivery behavior was verified against.
pub const MIN_INSTRUCTION_VERSION: (u64, u64, u64) = (0, 154, 0);

/// JSON-RPC carries the text; the registry budget bounds it far below this.
pub const MAX_INSTRUCTION_BYTES: usize = 256 * 1024;

fn upgrade_hint() -> String {
    let (major, minor, patch) = MIN_INSTRUCTION_VERSION;
    format!("Update Codex to {major}.{minor}.{patch} or newer")
}

/// Parse `codex --version` (`codex-cli 0.154.0`) into a support declaration.
pub fn parse_version(output: &str) -> InstructionSupport {
    let version = output.split_whitespace().find_map(|word| {
        let mut parts = word
            .trim_start_matches('v')
            .split(['.', '-'])
            .map(|p| p.parse::<u64>().ok());
        Some((parts.next()??, parts.next()??, parts.next()??))
    });
    match version {
        Some(version) if version >= MIN_INSTRUCTION_VERSION => InstructionSupport::Supported {
            channel: InstructionChannel::DeveloperInstructions,
            replace_strategy: InstructionReplaceStrategy::FreshSession,
            max_bytes: MAX_INSTRUCTION_BYTES,
        },
        Some((major, minor, patch)) => InstructionSupport::Unsupported {
            reason: format!(
                "Codex {major}.{minor}.{patch} predates verified Work Outline instruction delivery"
            ),
            upgrade_hint: Some(upgrade_hint()),
        },
        None => InstructionSupport::Unsupported {
            reason: "could not read the Codex version".into(),
            upgrade_hint: Some(upgrade_hint()),
        },
    }
}

pub(super) fn probe(exe: &Path) -> InstructionSupport {
    match crate::instruction_support::bounded_output(exe, &["--version"], Duration::from_secs(15)) {
        Ok(output) => parse_version(&output),
        Err(err) => InstructionSupport::Unsupported {
            reason: format!("could not check the Codex version: {err}"),
            upgrade_hint: Some(upgrade_hint()),
        },
    }
}

/// The `developerInstructions` value for `thread/start` AND `thread/resume`:
/// always the request's exact envelope text, never a different revision.
pub fn developer_instructions(request: &RunRequest) -> Option<&str> {
    request
        .instructions
        .as_ref()
        .map(|delivery| delivery.envelope.rendered.as_str())
}

/// Refuse a revision change aimed at an existing thread (strategy
/// `FreshSession`): the engine must start a new thread instead, after the
/// user confirmed the context reset. Never resume with a different value.
pub fn check_transition(request: &RunRequest) -> Result<(), HarnessError> {
    match (&request.instructions, &request.resume) {
        (Some(delivery), Some(_))
            if matches!(delivery.transition, InstructionTransition::Replace { .. }) =>
        {
            Err(HarnessError::Capability {
                harness: HarnessId::Codex,
                detail: "Codex cannot replace Work Outline instructions inside an existing \
                         thread; the new revision needs a fresh Codex session"
                    .into(),
                upgrade_hint: None,
            })
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verified_versions_are_supported_with_a_fresh_session_strategy() {
        assert_eq!(
            parse_version("codex-cli 0.154.0\n"),
            InstructionSupport::Supported {
                channel: InstructionChannel::DeveloperInstructions,
                replace_strategy: InstructionReplaceStrategy::FreshSession,
                max_bytes: MAX_INSTRUCTION_BYTES,
            }
        );
        assert!(parse_version("codex-cli 1.0.2").is_supported());
        assert!(parse_version("codex-cli 0.155.0-alpha.1").is_supported());
    }

    #[test]
    fn older_or_unreadable_versions_ask_for_an_upgrade() {
        for output in ["codex-cli 0.153.9", "garbage", ""] {
            match parse_version(output) {
                InstructionSupport::Unsupported { upgrade_hint, .. } => {
                    assert_eq!(
                        upgrade_hint.as_deref(),
                        Some("Update Codex to 0.154.0 or newer")
                    );
                }
                other => panic!("{output:?}: expected Unsupported, got {other:?}"),
            }
        }
    }
}
