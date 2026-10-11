use serde_json::json;

use super::*;
use crate::work_outline::{
    WorkItemEffectiveStatus, WorkItemRollup, WorkItemStatus, WorkOutlineMutation,
    WorkOutlineQueryRequest, WorkOutlineQueryResponse, WorkOutlineSnapshot,
};

const SPACE: &str = "space-1";

fn binding(repo: &str, primary: bool) -> RepositoryBinding {
    RepositoryBinding {
        repo_id: format!("repo:{repo}"),
        repo_fingerprint: format!("{repo}-root"),
        repo_path: format!("/ws/{repo}"),
        member: true,
        primary,
        worktree_id: None,
        worktree_path: None,
        expected_ref: None,
    }
}

fn config(root: &str, bindings: Vec<RepositoryBinding>) -> ExecutionConfiguration {
    ExecutionConfiguration {
        execution_root: root.into(),
        bindings,
        schema: EXECUTION_CONFIGURATION_SCHEMA,
    }
}

fn item(id: &str, parent: Option<&str>) -> WorkItemView {
    WorkItemView {
        id: id.into(),
        space_id: SPACE.into(),
        parent_work_item_id: parent.map(Into::into),
        raw_parent_work_item_id: parent.map(Into::into),
        order_key: "a0".into(),
        title: id.into(),
        description: None,
        status: WorkItemStatus::Pending,
        effective_status: WorkItemEffectiveStatus::Pending,
        archived_at: None,
        archived_by: None,
        archive_cause: None,
        inherit_parent_instructions: true,
        created_by: None,
        created_at: 0,
        updated_at: 0,
        child_ids: Vec::new(),
        depth: 0,
        chat_ids: Vec::new(),
        external_reference_ids: Vec::new(),
        rollup: WorkItemRollup::default(),
    }
}

fn row(
    scope: ExecutionScope,
    configuration: Option<ExecutionConfiguration>,
) -> ExecutionConfigurationView {
    let work_item_id = match &scope {
        ExecutionScope::WorkItem { work_item_id } => Some(work_item_id.clone()),
        ExecutionScope::Space { .. } => None,
    };
    ExecutionConfigurationView {
        id: scope.row_id(),
        space_id: SPACE.into(),
        work_item_id,
        configuration,
        invalid_reason: None,
        updated_by: None,
        updated_at: 0,
    }
}

fn space() -> ExecutionSpace {
    ExecutionSpace {
        space_id: SPACE.into(),
        path: "/ws".into(),
        device_id: "dev-a".into(),
    }
}

fn work_item(id: &str) -> ExecutionScope {
    ExecutionScope::WorkItem {
        work_item_id: id.into(),
    }
}

#[test]
fn configuration_json_field_names() {
    let mut b = binding("ui", true);
    b.worktree_id = Some("ck-1".into());
    b.worktree_path = Some("/ws/.worktrees/ui/t".into());
    b.expected_ref = Some("feature".into());
    let value = serde_json::to_value(config("/ws", vec![b])).unwrap();
    assert_eq!(
        value,
        json!({
            "executionRoot": "/ws",
            "bindings": [{
                "repoId": "repo:ui",
                "repoFingerprint": "ui-root",
                "repoPath": "/ws/ui",
                "member": true,
                "primary": true,
                "worktreeId": "ck-1",
                "worktreePath": "/ws/.worktrees/ui/t",
                "expectedRef": "feature"
            }],
            "schema": 1
        })
    );
    // Optional fields absent; schema defaults to 1.
    let decoded: ExecutionConfiguration = serde_json::from_value(json!({
        "executionRoot": "/ws",
        "bindings": [{"repoId": "repo:x", "repoPath": "/ws/x"}]
    }))
    .unwrap();
    assert_eq!(decoded.schema, 1);
    assert!(!decoded.bindings[0].member && !decoded.bindings[0].primary);
    assert_eq!(decoded.bindings[0].repo_fingerprint, "");
}

#[test]
fn scope_and_source_json() {
    assert_eq!(
        serde_json::to_value(ExecutionScope::Space {
            space_id: "s".into()
        })
        .unwrap(),
        json!({"type": "space", "spaceId": "s"})
    );
    assert_eq!(
        serde_json::to_value(work_item("w")).unwrap(),
        json!({"type": "workItem", "workItemId": "w"})
    );
    assert_eq!(
        serde_json::to_value(ExecutionSourceScope::ImplicitSpaceRoot {
            space_id: "s".into()
        })
        .unwrap(),
        json!({"type": "implicitSpaceRoot", "spaceId": "s"})
    );
    assert_eq!(
        serde_json::to_value(ExecutionSourceScope::SpaceDefault {
            space_id: "s".into()
        })
        .unwrap(),
        json!({"type": "spaceDefault", "spaceId": "s"})
    );
    // Lenient snapshot enum.
    let unknown: ExecutionSourceScope =
        serde_json::from_value(json!({"type": "fromTheFuture", "x": 1})).unwrap();
    assert_eq!(unknown, ExecutionSourceScope::Unknown);
    // Strict request enum.
    assert!(serde_json::from_value::<ExecutionScope>(json!({"type": "other"})).is_err());
}

#[test]
fn scope_row_ids_round_trip() {
    let space = ExecutionScope::Space {
        space_id: "abc".into(),
    };
    assert_eq!(space.row_id(), "s:abc");
    assert_eq!(work_item("w1").row_id(), "w:w1");
    assert_eq!(ExecutionScope::from_row_id("s:abc"), Some(space));
    assert_eq!(ExecutionScope::from_row_id("w:w1"), Some(work_item("w1")));
    assert_eq!(ExecutionScope::from_row_id("w:"), None);
    assert_eq!(ExecutionScope::from_row_id("x:1"), None);
}

#[test]
fn mutation_and_query_json() {
    let mutation = WorkOutlineMutation::ConfigureExecution(ConfigureExecutionParams {
        scope: work_item("w1"),
        configuration: None,
        operation_id: "op-1".into(),
    });
    let value = serde_json::to_value(&mutation).unwrap();
    assert_eq!(
        value,
        json!({
            "op": "configureExecution",
            "scope": {"type": "workItem", "workItemId": "w1"},
            "configuration": null,
            "operationId": "op-1"
        })
    );
    assert_eq!(
        serde_json::from_value::<WorkOutlineMutation>(value).unwrap(),
        mutation
    );
    assert_eq!(mutation.operation_id(), "op-1");
    assert!(!mutation.is_instruction_operation());

    let query = WorkOutlineQueryRequest::ResolveExecutionConfiguration {
        scope: work_item("w1"),
    };
    assert_eq!(
        serde_json::to_value(&query).unwrap(),
        json!({
            "query": "resolveExecutionConfiguration",
            "scope": {"type": "workItem", "workItemId": "w1"}
        })
    );
    let resolved = ResolvedExecutionConfiguration {
        space_id: SPACE.into(),
        device_id: "dev-a".into(),
        configuration: ExecutionConfiguration::implicit("/ws"),
        source_scope: ExecutionSourceScope::ImplicitSpaceRoot {
            space_id: SPACE.into(),
        },
        invalid_reason: None,
    };
    let response = WorkOutlineQueryResponse::ResolveExecutionConfiguration(resolved.clone());
    assert_eq!(
        serde_json::to_value(&response).unwrap(),
        json!({
            "query": "resolveExecutionConfiguration",
            "spaceId": SPACE,
            "deviceId": "dev-a",
            "configuration": {"executionRoot": "/ws", "bindings": [], "schema": 1},
            "sourceScope": {"type": "implicitSpaceRoot", "spaceId": SPACE}
        })
    );
}

#[test]
fn validation_error_json_and_texts() {
    let error = ExecutionValidationError::WorktreeMoved {
        repo_id: "repo:ui".into(),
        worktree_id: "ck".into(),
        expected_path: "/a".into(),
        found_path: "/b".into(),
    };
    assert_eq!(
        serde_json::to_value(&error).unwrap(),
        json!({
            "code": "worktreeMoved",
            "repoId": "repo:ui",
            "worktreeId": "ck",
            "expectedPath": "/a",
            "foundPath": "/b"
        })
    );
    assert_eq!(error.code(), "worktreeMoved");
    let report = error.clone().report();
    assert!(report.message.contains("/b"));
    assert!(!report.remediation.is_empty());
    let value = serde_json::to_value(&report).unwrap();
    assert_eq!(value["error"]["code"], "worktreeMoved");
    assert!(value["remediation"].is_string());
    let unknown: ExecutionValidationError =
        serde_json::from_value(json!({"code": "somethingNew"})).unwrap();
    assert_eq!(unknown, ExecutionValidationError::Unknown);
    // Every variant spells its code the way serde does.
    let all = vec![
        ExecutionValidationError::DeviceMismatch {
            expected_device_id: "a".into(),
            found_device_id: "b".into(),
        },
        ExecutionValidationError::DeviceOffline {
            device_id: "a".into(),
            detail: String::new(),
        },
        ExecutionValidationError::HostUnsupported {
            device_id: "a".into(),
            capability: "c".into(),
        },
        ExecutionValidationError::SpaceMissing {
            space_id: "s".into(),
        },
        ExecutionValidationError::RootMissing { path: "/r".into() },
        ExecutionValidationError::RootNotDirectory { path: "/r".into() },
        ExecutionValidationError::RepositoryMissing {
            repo_id: "r".into(),
            path: "/p".into(),
        },
        ExecutionValidationError::RepositoryIdentityMismatch {
            repo_id: "r".into(),
            path: "/p".into(),
            found_repo_id: None,
            expected_fingerprint: "a".into(),
            found_fingerprint: Some("b".into()),
        },
        ExecutionValidationError::WorktreeMissing {
            repo_id: "r".into(),
            worktree_id: "w".into(),
            path: "/p".into(),
        },
        ExecutionValidationError::WorktreePruned {
            repo_id: "r".into(),
            worktree_id: "w".into(),
            path: "/p".into(),
        },
        error,
        ExecutionValidationError::BranchMismatch {
            repo_id: "r".into(),
            path: "/p".into(),
            expected_ref: "main".into(),
            found_ref: "dev".into(),
        },
        ExecutionValidationError::MemberOutsideRoot {
            repo_id: "r".into(),
            path: "/p".into(),
            execution_root: "/root".into(),
        },
        ExecutionValidationError::MultiplePrimary {
            repo_ids: vec!["a".into(), "b".into()],
        },
        ExecutionValidationError::ConfigInvalid { reason: "x".into() },
    ];
    for error in all {
        let value = serde_json::to_value(&error).unwrap();
        assert_eq!(value["code"], error.code());
        assert_eq!(
            serde_json::from_value::<ExecutionValidationError>(value).unwrap(),
            error
        );
        assert!(!error.message().is_empty());
        assert!(!error.remediation().is_empty());
    }
}

#[test]
fn validate_params_and_reply_json() {
    let params: ValidateExecutionConfigurationParams =
        serde_json::from_value(json!({"chatId": "c1"})).unwrap();
    assert_eq!(params.chat_id.as_deref(), Some("c1"));
    assert!(!params.host_capabilities.additional_directories);
    let params: ValidateExecutionConfigurationParams = serde_json::from_value(json!({
        "scope": {"type": "space", "spaceId": "s"},
        "hostCapabilities": {"additionalDirectories": true},
        "targetDeviceId": "dev-b"
    }))
    .unwrap();
    assert!(params.host_capabilities.additional_directories);
    assert_eq!(params.target_device_id.as_deref(), Some("dev-b"));

    let reply = ValidateExecutionConfigurationReply::from_result(
        None,
        Err(vec![ExecutionValidationError::RootMissing {
            path: "/r".into(),
        }]),
    );
    let value = serde_json::to_value(&reply).unwrap();
    assert_eq!(value["ok"], false);
    assert_eq!(value["errors"][0]["error"]["code"], "rootMissing");
    assert!(value.get("validated").is_none());

    let validated = ValidatedExecution {
        space_id: "s".into(),
        device_id: "d".into(),
        execution_root: "/ws".into(),
        root_checkout_id: Some("ck-root".into()),
        repositories: vec![ValidatedRepository {
            repo_id: "repo:ui".into(),
            repo_path: "/ws/ui".into(),
            checkout_path: "/ws/.worktrees/ui/t".into(),
            checkout_id: "ck-ui".into(),
            member: true,
            primary: true,
            branch: Some("t".into()),
            head_sha: Some("abc".into()),
            worktree_configured: true,
        }],
        source_scope: ExecutionSourceScope::SpaceDefault {
            space_id: "s".into(),
        },
    };
    assert_eq!(
        serde_json::to_value(&validated).unwrap(),
        json!({
            "spaceId": "s",
            "deviceId": "d",
            "executionRoot": "/ws",
            "rootCheckoutId": "ck-root",
            "repositories": [{
                "repoId": "repo:ui",
                "repoPath": "/ws/ui",
                "checkoutPath": "/ws/.worktrees/ui/t",
                "checkoutId": "ck-ui",
                "member": true,
                "primary": true,
                "branch": "t",
                "headSha": "abc",
                "worktreeConfigured": true
            }],
            "sourceScope": {"type": "spaceDefault", "spaceId": "s"}
        })
    );
}

#[test]
fn binding_view_and_snapshot_fields() {
    let view = ChatExecutionBindingView {
        chat_id: "c1".into(),
        space_id: SPACE.into(),
        resolved: ResolvedExecutionConfiguration {
            space_id: SPACE.into(),
            device_id: "dev-a".into(),
            configuration: ExecutionConfiguration::implicit("/ws"),
            source_scope: ExecutionSourceScope::WorkItem {
                work_item_id: "w1".into(),
            },
            invalid_reason: None,
        },
        captured_at: 5,
        captured_by: None,
        operation_id: Some("op".into()),
        work_item_id: Some("w1".into()),
        exception: true,
    };
    let value = serde_json::to_value(&view).unwrap();
    for key in [
        "chatId",
        "spaceId",
        "resolved",
        "capturedAt",
        "operationId",
        "workItemId",
        "exception",
    ] {
        assert!(value.get(key).is_some(), "{key}");
    }
    let snapshot = WorkOutlineSnapshot {
        space_id: SPACE.into(),
        execution_space: Some(space()),
        chat_execution_bindings: vec![view],
        execution_exceptions: vec![WorkItemExecutionException {
            work_item_id: "w1".into(),
            chat_ids: vec!["c1".into()],
        }],
        ..Default::default()
    };
    let value = serde_json::to_value(&snapshot).unwrap();
    for key in [
        "executionSpace",
        "executionConfigurations",
        "chatExecutionBindings",
        "executionExceptions",
    ] {
        assert!(value.get(key).is_some(), "{key}");
    }
    assert_eq!(snapshot.execution_exception_chats("w1"), ["c1".to_owned()]);
    assert!(snapshot.execution_exception_chats("w2").is_empty());
    // An older snapshot without the fields still decodes.
    let old: WorkOutlineSnapshot =
        serde_json::from_value(json!({"spaceId": SPACE, "available": true})).unwrap();
    assert!(old.execution_configurations.is_empty() && old.execution_space.is_none());
    assert!(!snapshot.same_content_as(&old));
}

#[test]
fn shape_check_rejects_bad_configurations() {
    assert!(check_execution_configuration(&config("/ws", vec![binding("ui", true)])).is_ok());
    assert!(check_execution_configuration(&config("C:\\ws", vec![])).is_ok());
    assert!(check_execution_configuration(&config("relative", vec![])).is_err());
    assert!(check_execution_configuration(&config("", vec![])).is_err());
    let two = config("/ws", vec![binding("ui", true), binding("flagship", true)]);
    assert_eq!(
        check_execution_configuration(&two).unwrap_err(),
        "Only one repository can be the primary focus"
    );
    assert!(matches!(
        execution_configuration_shape_errors(&two).as_slice(),
        [ExecutionValidationError::MultiplePrimary { repo_ids }] if repo_ids.len() == 2
    ));
    let dup = config("/ws", vec![binding("ui", false), binding("ui", false)]);
    assert!(
        check_execution_configuration(&dup)
            .unwrap_err()
            .contains("twice")
    );
    let mut half = binding("ui", false);
    half.worktree_id = Some("ck".into());
    assert!(check_execution_configuration(&config("/ws", vec![half])).is_err());
    let mut future = config("/ws", vec![]);
    future.schema = 2;
    assert!(check_execution_configuration(&future).is_err());
    let many: Vec<RepositoryBinding> = (0..=execution_limits::MAX_BINDINGS)
        .map(|i| binding(&format!("r{i}"), false))
        .collect();
    assert!(check_execution_configuration(&config("/ws", many)).is_err());
}

#[test]
fn resolver_precedence_item_then_ancestor_then_space_then_implicit() {
    let items = vec![
        item("root", None),
        item("mid", Some("root")),
        item("leaf", Some("mid")),
    ];
    let leaf_cfg = config("/ws/leaf", vec![]);
    let mid_cfg = config("/ws/mid", vec![binding("ui", true)]);
    let space_cfg = config("/ws/space", vec![]);

    // Implicit when nothing is configured.
    let r = resolve_execution_configuration(&work_item("leaf"), &space(), &items, &[]).unwrap();
    assert_eq!(r.configuration, ExecutionConfiguration::implicit("/ws"));
    assert_eq!(
        r.source_scope,
        ExecutionSourceScope::ImplicitSpaceRoot {
            space_id: SPACE.into()
        }
    );
    assert_eq!(r.device_id, "dev-a");

    let mut rows = vec![row(
        ExecutionScope::Space {
            space_id: SPACE.into(),
        },
        Some(space_cfg.clone()),
    )];
    let r = resolve_execution_configuration(&work_item("leaf"), &space(), &items, &rows).unwrap();
    assert_eq!(r.configuration, space_cfg);
    assert!(matches!(
        r.source_scope,
        ExecutionSourceScope::SpaceDefault { .. }
    ));

    rows.push(row(work_item("mid"), Some(mid_cfg.clone())));
    let r = resolve_execution_configuration(&work_item("leaf"), &space(), &items, &rows).unwrap();
    assert_eq!(r.configuration, mid_cfg);
    assert_eq!(
        r.source_scope,
        ExecutionSourceScope::WorkItem {
            work_item_id: "mid".into()
        }
    );
    // The ancestor's row does not leak upward.
    let r = resolve_execution_configuration(&work_item("root"), &space(), &items, &rows).unwrap();
    assert_eq!(r.configuration, space_cfg);

    rows.push(row(work_item("leaf"), Some(leaf_cfg.clone())));
    let r = resolve_execution_configuration(&work_item("leaf"), &space(), &items, &rows).unwrap();
    // Whole row: the leaf's empty bindings do NOT inherit mid's binding.
    assert_eq!(r.configuration, leaf_cfg);
    assert!(r.configuration.bindings.is_empty());

    // Space scope ignores item rows.
    let r = resolve_execution_configuration(
        &ExecutionScope::Space {
            space_id: SPACE.into(),
        },
        &space(),
        &items,
        &rows,
    )
    .unwrap();
    assert_eq!(r.configuration, space_cfg);

    // Unknown item.
    assert!(resolve_execution_configuration(&work_item("nope"), &space(), &items, &rows).is_none());
}

#[test]
fn resolver_fails_closed_on_an_unreadable_nearer_row() {
    let items = vec![item("root", None), item("leaf", Some("root"))];
    let rows = vec![
        row(work_item("root"), Some(config("/ws/root", vec![]))),
        row(work_item("leaf"), None),
    ];
    let r = resolve_execution_configuration(&work_item("leaf"), &space(), &items, &rows).unwrap();
    assert!(r.invalid_reason.is_some());
    assert_eq!(
        r.source_scope,
        ExecutionSourceScope::WorkItem {
            work_item_id: "leaf".into()
        }
    );
}

#[test]
fn resolver_ignores_rows_and_parents_of_other_spaces() {
    let mut foreign_parent = item("p", None);
    foreign_parent.space_id = "other".into();
    let items = vec![foreign_parent, item("c", Some("p"))];
    let mut foreign_row = row(work_item("p"), Some(config("/elsewhere", vec![])));
    foreign_row.space_id = "other".into();
    let r =
        resolve_execution_configuration(&work_item("c"), &space(), &items, &[foreign_row]).unwrap();
    assert!(matches!(
        r.source_scope,
        ExecutionSourceScope::ImplicitSpaceRoot { .. }
    ));
}

#[test]
fn absolute_device_paths() {
    assert!(is_absolute_device_path("/a"));
    assert!(is_absolute_device_path("C:\\a"));
    assert!(is_absolute_device_path("d:/a"));
    assert!(is_absolute_device_path("\\\\server\\share"));
    assert!(!is_absolute_device_path("a/b"));
    assert!(!is_absolute_device_path("~/a"));
    assert!(!is_absolute_device_path(""));
}

#[test]
fn launch_worktree_fill_is_not_an_exception_but_other_changes_are() {
    let pin = |mut b: RepositoryBinding, path: &str| {
        b.worktree_id = Some("wt".into());
        b.worktree_path = Some(path.into());
        b
    };
    // Implicit root: the launch synthesized the root repository's binding.
    let implicit = config("/ws/ui", vec![]);
    let filled = config(
        "/ws/ui/.worktrees/workspace/t",
        vec![pin(
            RepositoryBinding {
                repo_path: "/ws/ui/".into(),
                ..binding("ui", true)
            },
            "/ws/ui/.worktrees/workspace/t",
        )],
    );
    assert!(is_launch_worktree_fill(&implicit, &filled));
    let mut constrained = filled.clone();
    constrained.bindings[0].expected_ref = Some("main".into());
    assert!(!is_launch_worktree_fill(&implicit, &constrained));
    let mut elsewhere = filled.clone();
    elsewhere.execution_root = "/somewhere".into();
    assert!(!is_launch_worktree_fill(&implicit, &elsewhere));

    // Bindings: one unpinned binding gains a pin; the root moves with it
    // only when the root was that repository's checkout.
    let multi = config("/ws", vec![binding("ui", true), binding("flagship", false)]);
    let pinned = config(
        "/ws",
        vec![
            pin(binding("ui", true), "/ws/.worktrees/ui/t"),
            binding("flagship", false),
        ],
    );
    assert!(is_launch_worktree_fill(&multi, &pinned));
    let moved = config("/ws/.worktrees/ui/t", pinned.bindings.clone());
    assert!(!is_launch_worktree_fill(&multi, &moved));
    let single = config("/ws/ui", vec![binding("ui", true)]);
    let single_moved = config(
        "/ws/ui/.worktrees/x",
        vec![pin(binding("ui", true), "/ws/ui/.worktrees/x")],
    );
    assert!(is_launch_worktree_fill(&single, &single_moved));
    // Two pins, a re-pin, or any other field change are exceptions.
    let both = config(
        "/ws",
        vec![
            pin(binding("ui", true), "/a"),
            pin(binding("flagship", false), "/b"),
        ],
    );
    assert!(!is_launch_worktree_fill(&multi, &both));
    assert!(!is_launch_worktree_fill(
        &pinned,
        &config(
            "/ws",
            vec![
                pin(binding("ui", true), "/other"),
                binding("flagship", false),
            ]
        )
    ));
    let mut member_changed = pinned.clone();
    member_changed.bindings[1].member = false;
    assert!(!is_launch_worktree_fill(&multi, &member_changed));
    assert!(!is_launch_worktree_fill(&multi, &multi));
}
