//! Start Chat from a work item (Milestone 3 Part B): pure, GUI-free logic
//! behind the Shell's Start Chat dialog (`shell/start_chat_ui.rs`).
//!
//! Wire: `CreateChatForWorkItem` (forwardable; the engine forwards to the
//! Space's owner itself) — `crates/proto/src/work_outline_launch.rs` and the
//! contract's "Milestone 3 launch discoveries". A failed or unknown-outcome
//! start is retried with the SAME operation id while the inputs are
//! unchanged (`OperationSlot`); the Chat is never deleted on a partial
//! failure.
use zeron_proto::{
    ExecutionConfiguration, ExecutionValidationError, ResolvedExecutionConfiguration,
    WorkOutlineError, WorkOutlineErrorCode,
};

use super::store::WorkOutlineSnapshot;

/// Prefix of the engine's execution rejection text
/// (`execution_scope::rejection_message`).
pub(crate) const REJECTION_PREFIX: &str = "execution-scope:";

/// The work item's most recent launch: the associated Chat with a captured
/// execution binding (bindings are captured at launch) and the latest
/// `capturedAt`. `None` when the item was never launched.
pub(crate) fn last_launch_chat<'a>(
    snapshot: &'a WorkOutlineSnapshot,
    item_id: &str,
) -> Option<&'a str> {
    snapshot
        .chat_execution_bindings
        .iter()
        .filter(|binding| binding.work_item_id.as_deref() == Some(item_id))
        .max_by(|a, b| {
            a.captured_at
                .cmp(&b.captured_at)
                .then_with(|| a.chat_id.cmp(&b.chat_id))
        })
        .map(|binding| binding.chat_id.as_str())
}

/// Whether the item's effective instructions changed since its last launch:
/// the host's drift for the last launched Chat's selected revision (which
/// compares instruction content, so item title renames such as Linear sync
/// never count), or it launched without instructions and the item has some
/// now. Never launched = not changed.
pub(crate) fn instructions_changed_since_last_launch(
    snapshot: &WorkOutlineSnapshot,
    item_id: &str,
) -> bool {
    let Some(chat_id) = last_launch_chat(snapshot, item_id) else {
        return false;
    };
    let has_entries = !snapshot.effective_instructions(item_id).entries.is_empty();
    match snapshot.chat_instruction_state(chat_id) {
        Some(state) if state.selected_revision_hash.is_some() => state.drift,
        _ => has_entries,
    }
}

/// Which repository a worktree choice applies to (engine rule: the primary
/// binding, else the only binding, else the repository at the execution
/// root).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorktreeTarget {
    /// No choice applies: the target already pins a worktree, or several
    /// bindings have no primary.
    None,
    /// An unpinned binding; describe `repo_path`.
    Binding { repo_path: String },
    /// No bindings: the repository at the execution root, if it is one.
    Root { path: String },
    /// The target repository's checkout is not the execution root, so a
    /// chosen worktree could not become the root (the engine refuses the
    /// choice); the dialog explains instead of offering it.
    Unavailable { reason: String },
}

impl WorktreeTarget {
    /// The path to describe for the worktree choices.
    pub(crate) fn describe_path(&self) -> Option<&str> {
        match self {
            Self::None | Self::Unavailable { .. } => None,
            Self::Binding { repo_path } => Some(repo_path),
            Self::Root { path } => Some(path),
        }
    }
}

pub(crate) fn worktree_target(configuration: &ExecutionConfiguration) -> WorktreeTarget {
    let target = configuration
        .bindings
        .iter()
        .find(|binding| binding.primary)
        .or_else(|| match configuration.bindings.as_slice() {
            [only] => Some(only),
            _ => None,
        });
    match target {
        Some(binding) if binding.worktree_id.is_some() => WorktreeTarget::None,
        Some(binding)
            if binding.repo_path.trim_end_matches('/')
                != configuration.execution_root.trim_end_matches('/') =>
        {
            WorktreeTarget::Unavailable {
                reason: format!(
                    "A worktree can only be chosen when the execution root is the primary \
                     repository's checkout. This item runs in {}, not {}.",
                    configuration.execution_root, binding.repo_path
                ),
            }
        }
        Some(binding) => WorktreeTarget::Binding {
            repo_path: binding.repo_path.clone(),
        },
        None if configuration.bindings.is_empty() && !configuration.execution_root.is_empty() => {
            WorktreeTarget::Root {
                path: configuration.execution_root.clone(),
            }
        }
        None => WorktreeTarget::None,
    }
}

/// The worktree the launch runs in when the configuration pins none.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum WorktreePick {
    /// The repository path / Space workspace as configured (no
    /// `worktreeChoice`).
    #[default]
    SpaceWorkspace,
    Existing {
        worktree_id: String,
        worktree_path: String,
    },
    /// Create `.worktrees/<label>/<slug(name)>` with the existing flow.
    CreateNew,
}

/// The `worktreeChoice` for a pick (`name` is the new worktree's name).
pub(crate) fn worktree_choice(
    pick: &WorktreePick,
    name: &str,
) -> Result<Option<zeron_proto::WorktreeChoice>, String> {
    match pick {
        WorktreePick::SpaceWorkspace => Ok(None),
        WorktreePick::Existing {
            worktree_id,
            worktree_path,
        } => Ok(Some(zeron_proto::WorktreeChoice::UseExisting {
            worktree_id: worktree_id.clone(),
            worktree_path: worktree_path.clone(),
        })),
        WorktreePick::CreateNew => {
            let name = name.trim();
            if name.is_empty() {
                Err("Name the new worktree".into())
            } else {
                Ok(Some(zeron_proto::WorktreeChoice::CreateNew {
                    name: name.to_owned(),
                }))
            }
        }
    }
}

/// A failed start, as the dialog shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LaunchFailure {
    /// The execution validation code when the host blocked on validation.
    pub code: Option<String>,
    pub message: String,
    pub remediation: Option<String>,
    /// A transport failure: the outcome is unknown; retrying with the same
    /// operation id is safe and completes whatever the host recorded.
    pub unknown_outcome: bool,
}

/// Every remediation text of the shared vocabulary (to split a rejection's
/// `<message>. <remediation>`).
fn known_remediations() -> Vec<&'static str> {
    use ExecutionValidationError as E;
    let s = String::new;
    [
        E::DeviceMismatch {
            expected_device_id: s(),
            found_device_id: s(),
        },
        E::DeviceOffline {
            device_id: s(),
            detail: s(),
        },
        E::HostUnsupported {
            device_id: s(),
            capability: s(),
        },
        E::SpaceMissing { space_id: s() },
        E::RootMissing { path: s() },
        E::HomeDirectoryRoot { path: s() },
        E::RepositoryMissing {
            repo_id: s(),
            path: s(),
        },
        E::RepositoryIdentityMismatch {
            repo_id: s(),
            path: s(),
            found_repo_id: None,
            expected_fingerprint: s(),
            found_fingerprint: None,
        },
        E::WorktreeMissing {
            repo_id: s(),
            worktree_id: s(),
            path: s(),
        },
        E::WorktreePruned {
            repo_id: s(),
            worktree_id: s(),
            path: s(),
        },
        E::WorktreeMoved {
            repo_id: s(),
            worktree_id: s(),
            expected_path: s(),
            found_path: s(),
        },
        E::BranchMismatch {
            repo_id: s(),
            path: s(),
            expected_ref: s(),
            found_ref: s(),
        },
        E::MemberOutsideRoot {
            repo_id: s(),
            path: s(),
            execution_root: s(),
        },
        E::MultiplePrimary { repo_ids: vec![] },
        E::ConfigInvalid { reason: s() },
        E::malformed_chat_binding(""),
        E::ResumeRootMismatch {
            expected_path: s(),
            found_path: s(),
        },
        E::BoundChatRetarget {
            chat_id: s(),
            execution_root: s(),
            requested_path: s(),
        },
    ]
    .iter()
    .map(ExecutionValidationError::remediation)
    .collect()
}

/// Read a failed `CreateChatForWorkItem`: an `execution-scope:<code>:
/// <message>. <remediation> (+N more)` rejection splits into its parts;
/// anything else keeps its message.
pub(crate) fn launch_failure(error: &WorkOutlineError) -> LaunchFailure {
    let unknown_outcome = super::actions::is_transport_failure(error);
    let text = error.message.trim();
    if let Some(rest) = text.strip_prefix(REJECTION_PREFIX)
        && let Some((code, body)) = rest.split_once(": ")
    {
        let (body, more) = match body.rfind(" (+") {
            Some(ix) if body.ends_with(" more)") => (&body[..ix], Some(&body[ix + 1..])),
            _ => (body, None),
        };
        let remediation = known_remediations()
            .into_iter()
            .filter(|remediation| body.ends_with(remediation))
            .max_by_key(|remediation| remediation.len());
        let (message, remediation) = match remediation {
            Some(remediation) => {
                let message = body[..body.len() - remediation.len()]
                    .trim_end()
                    .trim_end_matches('.')
                    .to_owned();
                (message, Some(remediation.to_owned()))
            }
            None => (body.to_owned(), None),
        };
        let message = match more {
            Some(more) => format!("{message} {more}"),
            None => message,
        };
        return LaunchFailure {
            code: Some(code.to_owned()),
            message,
            remediation,
            unknown_outcome,
        };
    }
    let message = if super::actions::is_unsupported(error) {
        "Starting Chats from work items isn\u{2019}t available on this host yet".to_owned()
    } else if error.code == WorkOutlineErrorCode::Capability && !text.is_empty() {
        text.to_owned()
    } else if text.is_empty() {
        error.code.as_str().to_owned()
    } else {
        text.to_owned()
    };
    LaunchFailure {
        code: None,
        message,
        remediation: unknown_outcome
            .then(|| "Try again: the retry completes the same start, never a second Chat.".into()),
        unknown_outcome,
    }
}

/// One-line summary of where the Chat will run.
pub(crate) fn execution_summary(resolved: &ResolvedExecutionConfiguration) -> String {
    super::execution::captured_summary(&resolved.configuration)
}

#[cfg(test)]
mod tests {
    use super::super::execution::fixture as execution_fixture;
    use super::super::store::fixture_ids::*;
    use super::*;

    #[test]
    fn last_launch_and_instruction_change() {
        let mut snapshot = execution_fixture::snapshot("s1");
        assert_eq!(last_launch_chat(&snapshot, API), Some(CHAT_API));
        assert_eq!(last_launch_chat(&snapshot, HOUSEKEEPING), None);
        // Never launched: not changed.
        assert!(!instructions_changed_since_last_launch(
            &snapshot,
            HOUSEKEEPING
        ));
        // Launched without instructions, none now: not changed.
        assert!(!instructions_changed_since_last_launch(&snapshot, API));
        // Instructions added since: changed.
        let instructions = super::super::instructions::fixture::snapshot("s1");
        snapshot.instructions = instructions.instructions.clone();
        snapshot.chat_instruction_states = instructions.chat_instruction_states.clone();
        // chat-api applied an older API text (drifted fixture).
        assert!(instructions_changed_since_last_launch(&snapshot, API));
        // chat-auth applied the current Auth revision.
        assert!(!instructions_changed_since_last_launch(&snapshot, API_AUTH));
        // A changed hash the host does not call drift (an item title
        // rename, e.g. Linear sync) is not a change.
        for state in snapshot.chat_instruction_states.iter_mut() {
            if state.chat_id == CHAT_API {
                state.drift = false;
                state.current_effective_hash = Some("renamed-title-hash".into());
            }
        }
        assert!(!instructions_changed_since_last_launch(&snapshot, API));
    }

    #[test]
    fn worktree_target_follows_the_engine_rule() {
        use execution_fixture::{api_configuration, binding, launch_configuration};
        // The primary member's checkout is not the root: no worktree choice.
        let WorktreeTarget::Unavailable { reason } = worktree_target(&launch_configuration())
        else {
            panic!("the root is the superproject, not the ui checkout");
        };
        assert!(reason.contains("/work/alpha/ui"), "{reason}");
        assert_eq!(
            worktree_target(&launch_configuration()).describe_path(),
            None
        );
        assert_eq!(
            worktree_target(&api_configuration()),
            WorktreeTarget::Binding {
                repo_path: "/work/alpha/flagship".into()
            }
        );
        let mut pinned = api_configuration();
        pinned.bindings[0].worktree_id = Some("wt".into());
        pinned.bindings[0].worktree_path = Some("/wt".into());
        assert_eq!(worktree_target(&pinned), WorktreeTarget::None);
        let mut no_primary = launch_configuration();
        no_primary.bindings[0].primary = false;
        assert_eq!(worktree_target(&no_primary), WorktreeTarget::None);
        let only = ExecutionConfiguration {
            execution_root: "/r/x/".into(),
            bindings: vec![binding("repo:x", "/r/x", true, false)],
            schema: 1,
        };
        assert_eq!(
            worktree_target(&only).describe_path(),
            Some("/r/x"),
            "the only binding"
        );
        assert_eq!(
            worktree_target(&ExecutionConfiguration::implicit("/work/alpha")),
            WorktreeTarget::Root {
                path: "/work/alpha".into()
            }
        );
        assert_eq!(
            worktree_choice(&WorktreePick::CreateNew, "  "),
            Err("Name the new worktree".into())
        );
        assert_eq!(
            worktree_choice(&WorktreePick::CreateNew, "fix login").unwrap(),
            Some(zeron_proto::WorktreeChoice::CreateNew {
                name: "fix login".into()
            })
        );
        assert_eq!(
            worktree_choice(&WorktreePick::SpaceWorkspace, "").unwrap(),
            None
        );
    }

    #[test]
    fn rejections_split_into_message_and_remediation() {
        let error = ExecutionValidationError::WorktreeMissing {
            repo_id: "repo:ui".into(),
            worktree_id: "wt".into(),
            path: "/work/alpha/.worktrees/x".into(),
        };
        let wire = format!(
            "{REJECTION_PREFIX}{}: {}. {} (+1 more)",
            error.code(),
            error.message(),
            error.remediation()
        );
        let failure = launch_failure(&WorkOutlineError::new(WorkOutlineErrorCode::Invalid, wire));
        assert_eq!(failure.code.as_deref(), Some("worktreeMissing"));
        assert_eq!(
            failure.message,
            "The configured worktree /work/alpha/.worktrees/x is missing (+1 more)"
        );
        assert_eq!(failure.remediation.as_deref(), Some(error.remediation()));
        assert!(!failure.unknown_outcome);

        let transport = launch_failure(&WorkOutlineError::new(
            WorkOutlineErrorCode::Unsupported,
            format!(
                "{}: closed",
                super::super::actions::TRANSPORT_FAILURE_PREFIX
            ),
        ));
        assert!(transport.unknown_outcome);
        assert!(
            transport
                .remediation
                .unwrap()
                .contains("never a second Chat")
        );

        let unsupported = launch_failure(&WorkOutlineError::new(
            WorkOutlineErrorCode::Unsupported,
            "This host does not support Work Outline (CreateChatForWorkItem)",
        ));
        assert!(unsupported.message.contains("isn\u{2019}t available"));
    }
}
