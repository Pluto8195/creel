//! Work Outline instruction delivery for Claude Code (`m0-instruction-delivery.md`
//! sections 2 and 5, verified cells C4, C5, C8, C10).
//!
//! The rendered envelope rides `--append-system-prompt-file <zeron-owned file>`
//! together with `--system-prompt-snapshot off` on EVERY spawn of an
//! instruction-bearing session (fresh, resume, crash respawn): without `off`
//! the CLI records the first request's system prompt and a resume silently
//! keeps the old text (C3, C8); without re-supplying the file a resume runs
//! with no instructions at all (C5).
//!
//! Support is probed once per executable identity from `claude --help`: both
//! flags must be listed. `--version` cannot be the probe because the CLI
//! ignores unknown flags when printing its version.

use std::path::{Path, PathBuf};
use std::time::Duration;

use zeron_proto::{InstructionEnvelope, envelope_sha256};

use crate::{HarnessError, InstructionChannel, InstructionReplaceStrategy, InstructionSupport};

/// The Claude Code version the M0 spike verified the flags against.
pub const MIN_INSTRUCTION_VERSION: &str = "2.1.285";

/// Generous ceiling: the registry budget (9,000 effective characters plus the
/// preamble and labels) stays far below it, and the file channel has no argv
/// limit.
pub const MAX_INSTRUCTION_BYTES: usize = 256 * 1024;

const HELP_TIMEOUT: Duration = Duration::from_secs(15);

fn upgrade_hint() -> String {
    format!("Update Claude Code to {MIN_INSTRUCTION_VERSION} or newer")
}

/// Whether `claude --help` output lists both delivery flags.
pub fn parse_help(help: &str) -> InstructionSupport {
    let snapshot = help.contains("--system-prompt-snapshot");
    // 2.1.285 lists the file variant only in prose
    // (`--append-system-prompt[-file]`); accept either spelling.
    let file = help.contains("--append-system-prompt-file")
        || help.contains("--append-system-prompt[-file]");
    if snapshot && file {
        InstructionSupport::Supported {
            channel: InstructionChannel::SystemPromptAppend,
            replace_strategy: InstructionReplaceStrategy::RespawnResume,
            max_bytes: MAX_INSTRUCTION_BYTES,
        }
    } else {
        InstructionSupport::Unsupported {
            reason: "this Claude Code version cannot pin Work Outline instructions across \
                     resumes (it lacks --system-prompt-snapshot or \
                     --append-system-prompt-file)"
                .into(),
            upgrade_hint: Some(upgrade_hint()),
        }
    }
}

/// Run `<exe> --help` (bounded) and parse it.
pub(super) fn probe(exe: &Path) -> InstructionSupport {
    match help_text(exe) {
        Ok(help) => parse_help(&help),
        Err(err) => InstructionSupport::Unsupported {
            reason: format!("could not check Claude Code's instruction flags: {err}"),
            upgrade_hint: Some(upgrade_hint()),
        },
    }
}

fn help_text(exe: &Path) -> std::io::Result<String> {
    crate::instruction_support::bounded_output(exe, &["--help"], HELP_TIMEOUT)
}

/// Default Zeron-owned directory for envelope files: beside the profile's
/// model-discovery directory (`<store_root>/instructions`), else a private
/// temp directory. Never the user's checkout.
pub(super) fn default_dir(model_discovery_cwd: Option<&Path>) -> PathBuf {
    match model_discovery_cwd.and_then(Path::parent) {
        Some(store_root) => store_root.join("instructions"),
        None => std::env::temp_dir().join(format!("zeron-instructions-{}", std::process::id())),
    }
}

/// Write `envelope.rendered` byte-for-byte to `<dir>/<envelopeSha256>.md`
/// (owner-only on unix) and return the path. Content-addressed: every
/// Execution Session of one revision shares one immutable file, rewritten
/// atomically when missing or not byte-identical. The digest is re-checked
/// first so a mislabeled envelope can never be written under another name.
pub fn write_envelope_file(
    dir: &Path,
    envelope: &InstructionEnvelope,
) -> Result<PathBuf, HarnessError> {
    let digest = envelope_sha256(&envelope.rendered);
    if digest != envelope.envelope_sha256 {
        return Err(HarnessError::Capability {
            harness: zeron_proto::HarnessId::ClaudeCode,
            detail: "the rendered instructions do not match their envelope hash".into(),
            upgrade_hint: None,
        });
    }
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{digest}.md"));
    if std::fs::read(&path).is_ok_and(|bytes| bytes == envelope.rendered.as_bytes()) {
        return Ok(path);
    }
    let tmp = dir.join(format!(".{digest}.{}.tmp", uuid::Uuid::new_v4()));
    {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(envelope.rendered.as_bytes())?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, &path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(path)
}

/// The argv tail every instruction-bearing spawn carries.
pub fn delivery_args(file: &Path) -> Vec<std::ffi::OsString> {
    vec![
        "--append-system-prompt-file".into(),
        file.as_os_str().to_owned(),
        "--system-prompt-snapshot".into(),
        "off".into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(rendered: &str) -> InstructionEnvelope {
        InstructionEnvelope {
            revision_id: "rev".into(),
            revision_hash: "hash".into(),
            envelope_version: 1,
            envelope_sha256: envelope_sha256(rendered),
            rendered: rendered.into(),
        }
    }

    #[test]
    fn help_with_both_flags_is_supported() {
        let help = "  --append-system-prompt <prompt>  Append\n  via: --system-prompt[-file],\n  \
                    --append-system-prompt[-file], --add-dir\n  --system-prompt-snapshot <on|off>  Record";
        assert_eq!(
            parse_help(help),
            InstructionSupport::Supported {
                channel: InstructionChannel::SystemPromptAppend,
                replace_strategy: InstructionReplaceStrategy::RespawnResume,
                max_bytes: MAX_INSTRUCTION_BYTES,
            }
        );
        assert!(
            parse_help("--append-system-prompt-file x --system-prompt-snapshot").is_supported()
        );
    }

    #[test]
    fn help_without_the_snapshot_flag_asks_for_an_upgrade() {
        let support = parse_help("--append-system-prompt <prompt>\n--append-system-prompt-file");
        match support {
            InstructionSupport::Unsupported { upgrade_hint, .. } => {
                assert_eq!(
                    upgrade_hint.as_deref(),
                    Some("Update Claude Code to 2.1.285 or newer")
                );
            }
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn envelope_files_are_content_addressed_exact_and_private() {
        let dir = tempfile::tempdir().unwrap();
        let env = envelope(
            "<zeron_work_outline_instructions>\n1. Be terse\n</zeron_work_outline_instructions>\n",
        );
        let path = write_envelope_file(dir.path(), &env).unwrap();
        assert_eq!(path, dir.path().join(format!("{}.md", env.envelope_sha256)));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), env.rendered);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // A tampered file is rewritten, not trusted.
        std::fs::write(&path, "tampered").unwrap();
        assert_eq!(write_envelope_file(dir.path(), &env).unwrap(), path);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), env.rendered);
    }

    #[test]
    fn a_mislabeled_envelope_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = envelope("text");
        env.envelope_sha256 = "0".repeat(64);
        assert!(matches!(
            write_envelope_file(dir.path(), &env),
            Err(HarnessError::Capability { .. })
        ));
    }

    #[test]
    fn default_dir_sits_beside_model_discovery() {
        assert_eq!(
            default_dir(Some(Path::new("/profile/store/model-discovery"))),
            PathBuf::from("/profile/store/instructions")
        );
    }
}
