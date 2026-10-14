//! Wire-shape tests for the Linear contract: JSON field names, leniency,
//! error strings, the title/description format, and the Work Outline ops.

use serde_json::json;

use super::*;
use crate::work_outline::{
    CreateWorkItemFromLinearParams, LinkLinearProjectParams, SetSpaceLinearEnabledParams,
    WorkItemView, WorkOutlineMutation, WorkOutlinePlacement, WorkOutlineSnapshot,
    WorkOutlineSpaceSettings,
};

fn round_trip<T>(value: &T, expected: serde_json::Value)
where
    T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    assert_eq!(serde_json::to_value(value).unwrap(), expected);
    assert_eq!(&serde_json::from_value::<T>(expected).unwrap(), value);
}

fn issue() -> LinearIssueSummary {
    LinearIssueSummary {
        id: "uuid-1".into(),
        identifier: "ENG-123".into(),
        title: "Fix login".into(),
        description: Some("Steps".into()),
        url: "https://linear.app/acme/issue/ENG-123/fix-login".into(),
        state: LinearState {
            id: "st-1".into(),
            name: "In Progress".into(),
            state_type: LinearStateType::Started,
            color: "#f2c94c".into(),
        },
        assignee: Some(LinearUser {
            id: "u-1".into(),
            name: "Ada Lovelace".into(),
            display_name: Some("ada".into()),
            email: Some("ada@example.com".into()),
        }),
        priority: 2,
        priority_label: "High".into(),
        team: LinearTeamRef {
            id: "t-1".into(),
            key: "ENG".into(),
            name: "Engineering".into(),
        },
        project: Some(LinearProjectRef {
            id: "p-1".into(),
            name: "Auth".into(),
            url: "https://linear.app/acme/project/auth".into(),
        }),
        labels: vec![LinearLabel {
            name: "bug".into(),
            color: "#eb5757".into(),
        }],
        branch_name: Some("ada/eng-123-fix-login".into()),
        updated_at: "2026-10-13T10:00:00.000Z".into(),
    }
}

#[test]
fn issue_summary_field_names_are_camel_case() {
    round_trip(
        &issue(),
        json!({
            "id": "uuid-1",
            "identifier": "ENG-123",
            "title": "Fix login",
            "description": "Steps",
            "url": "https://linear.app/acme/issue/ENG-123/fix-login",
            "state": {"id": "st-1", "name": "In Progress", "type": "started", "color": "#f2c94c"},
            "assignee": {"id": "u-1", "name": "Ada Lovelace", "displayName": "ada", "email": "ada@example.com"},
            "priority": 2,
            "priorityLabel": "High",
            "team": {"id": "t-1", "key": "ENG", "name": "Engineering"},
            "project": {"id": "p-1", "name": "Auth", "url": "https://linear.app/acme/project/auth"},
            "labels": [{"name": "bug", "color": "#eb5757"}],
            "branchName": "ada/eng-123-fix-login",
            "updatedAt": "2026-10-13T10:00:00.000Z",
        }),
    );
}

#[test]
fn unknown_state_types_and_sparse_issues_decode() {
    let parsed: LinearIssueSummary = serde_json::from_value(json!({
        "id": "x", "identifier": "ENG-1", "state": {"type": "someday"}
    }))
    .unwrap();
    assert_eq!(parsed.state.state_type, LinearStateType::Unknown);
    assert!(parsed.labels.is_empty() && parsed.assignee.is_none());
    for (value, expected) in [
        ("triage", LinearStateType::Triage),
        ("backlog", LinearStateType::Backlog),
        ("unstarted", LinearStateType::Unstarted),
        ("started", LinearStateType::Started),
        ("completed", LinearStateType::Completed),
        ("canceled", LinearStateType::Canceled),
    ] {
        assert_eq!(
            serde_json::from_value::<LinearStateType>(json!(value)).unwrap(),
            expected
        );
        assert_eq!(serde_json::to_value(expected).unwrap(), json!(value));
    }
    assert!(LinearStateType::Canceled.is_closed() && !LinearStateType::Started.is_closed());
}

#[test]
fn project_and_status_shapes() {
    round_trip(
        &LinearProjectSummary {
            id: "p-1".into(),
            name: "Auth".into(),
            url: "https://linear.app/acme/project/auth".into(),
            state: Some("started".into()),
            lead: Some(LinearUser {
                id: "u-1".into(),
                name: "Ada".into(),
                display_name: None,
                email: None,
            }),
            open_issue_count: Some(4),
            updated_at: "2026-10-13T10:00:00.000Z".into(),
        },
        json!({
            "id": "p-1", "name": "Auth", "url": "https://linear.app/acme/project/auth",
            "state": "started", "lead": {"id": "u-1", "name": "Ada"},
            "openIssueCount": 4, "updatedAt": "2026-10-13T10:00:00.000Z",
        }),
    );
    round_trip(
        &LinearConnectionStatus {
            configured: true,
            source: LinearKeySource::Keychain,
            viewer: Some(LinearUser {
                id: "u-1".into(),
                name: "Ada".into(),
                display_name: None,
                email: Some("ada@example.com".into()),
            }),
            error: None,
        },
        json!({
            "configured": true, "source": "keychain",
            "viewer": {"id": "u-1", "name": "Ada", "email": "ada@example.com"},
        }),
    );
    round_trip(
        &LinearConnectionStatus::default(),
        json!({"configured": false, "source": "none"}),
    );
    assert_eq!(
        serde_json::to_value(LinearKeySource::Environment).unwrap(),
        json!("environment")
    );
    let odd: LinearConnectionStatus = serde_json::from_value(json!({"source": "vault"})).unwrap();
    assert_eq!(odd.source, LinearKeySource::None);
}

#[test]
fn rpc_params_field_names() {
    round_trip(
        &LinearSetApiKeyParams {
            api_key: "lin_api_x".into(),
        },
        json!({"apiKey": "lin_api_x"}),
    );
    round_trip(
        &LinearIssueParams {
            identifier: "ENG-1".into(),
            force_refresh: true,
        },
        json!({"identifier": "ENG-1", "forceRefresh": true}),
    );
    round_trip(
        &LinearIssueParams {
            identifier: "ENG-1".into(),
            force_refresh: false,
        },
        json!({"identifier": "ENG-1"}),
    );
    round_trip(
        &LinearProjectIssuesParams {
            project_id: "p-1".into(),
        },
        json!({"projectId": "p-1"}),
    );
    round_trip(
        &LinearRefreshLinkedItemsParams {
            space_id: "s-1".into(),
        },
        json!({"spaceId": "s-1"}),
    );
    round_trip(
        &LinearRefreshLinkedItemsReply { updated: 3 },
        json!({"updated": 3}),
    );
    round_trip(&LinearStatusParams::default(), json!({}));
    round_trip(&LinearMyIssuesParams::default(), json!({}));
    round_trip(&LinearMyProjectsParams::default(), json!({}));
    round_trip(&LinearClearApiKeyParams::default(), json!({}));
}

#[test]
fn set_api_key_debug_never_shows_the_key() {
    let params = LinearSetApiKeyParams {
        api_key: "lin_api_secret_value".into(),
    };
    let debug = format!("{params:?}");
    assert!(!debug.contains("lin_api_secret_value"), "{debug}");
    assert!(debug.contains("redacted"));
}

#[test]
fn errors_round_trip_through_wire_strings() {
    for code in [
        LinearErrorCode::MissingKey,
        LinearErrorCode::Unauthorized,
        LinearErrorCode::RateLimited,
        LinearErrorCode::NotFound,
        LinearErrorCode::Network,
        LinearErrorCode::DisabledForSpace,
        LinearErrorCode::Invalid,
    ] {
        let error = LinearError::new(code, "boom: detail");
        let wire = error.to_wire_message();
        assert!(wire.starts_with(&format!("linear/{}: ", code.as_str())));
        assert_eq!(LinearError::from_wire_message(&wire), Some(error));
        assert_eq!(
            serde_json::to_value(code).unwrap(),
            json!(code.as_str())
        );
    }
    assert_eq!(
        LinearError::disabled_for_space().to_wire_message(),
        "linear/disabledForSpace: Linear is turned off for this Space"
    );
    assert_eq!(LinearError::from_wire_message("work-outline/invalid: x"), None);
    assert_eq!(
        LinearError::from_wire_message("linear/teleported: x").unwrap().code,
        LinearErrorCode::Invalid
    );
}

#[test]
fn item_title_and_description_format() {
    assert_eq!(linear_item_title("ENG-123", "Fix login"), "ENG-123 · Fix login");
    assert_eq!(
        linear_item_title(" ENG-1 ", "  multi\n line  title "),
        "ENG-1 · multi line title"
    );
    assert_eq!(linear_item_title("ENG-1", ""), "ENG-1");
    let long = linear_item_title("ENG-1", &"x".repeat(1_000));
    assert_eq!(long.chars().count(), limits::MAX_TITLE_CHARS);
    assert!(long.ends_with('…'));

    assert_eq!(linear_item_description(None), None);
    assert_eq!(linear_item_description(Some("  \n")), None);
    assert_eq!(linear_item_description(Some("Body\n")).as_deref(), Some("Body"));
    let huge = linear_item_description(Some(&"y".repeat(20_000))).unwrap();
    assert_eq!(huge.chars().count(), limits::MAX_DESCRIPTION_CHARS);
    // Escape-heavy text stays inside one registry op.
    let quotes = linear_item_description(Some(&"\"".repeat(8_000))).unwrap();
    let encoded = serde_json::to_string(&quotes).unwrap().encode_utf16().count();
    assert!(encoded <= 12_000, "{encoded}");
}

#[test]
fn work_outline_linear_operations_wire_shape() {
    let create = WorkOutlineMutation::CreateWorkItemFromLinear(CreateWorkItemFromLinearParams {
        space_id: "s-1".into(),
        parent_work_item_id: Some("w-1".into()),
        placement: WorkOutlinePlacement::Last,
        issue: LinearIssueLink::from(&issue()),
        operation_id: "op-1".into(),
    });
    let value = serde_json::to_value(&create).unwrap();
    assert_eq!(value["op"], json!("createWorkItemFromLinear"));
    assert_eq!(value["spaceId"], json!("s-1"));
    assert_eq!(value["parentWorkItemId"], json!("w-1"));
    assert_eq!(value["operationId"], json!("op-1"));
    assert_eq!(
        value["issue"],
        json!({
            "id": "uuid-1", "identifier": "ENG-123", "title": "Fix login",
            "description": "Steps", "url": "https://linear.app/acme/issue/ENG-123/fix-login",
        })
    );
    assert_eq!(
        serde_json::from_value::<WorkOutlineMutation>(value).unwrap(),
        create
    );
    assert_eq!(create.op_name(), "createWorkItemFromLinear");
    assert!(create.requires_linear_enabled());

    let link = WorkOutlineMutation::LinkLinearProject(LinkLinearProjectParams {
        work_item_id: "w-1".into(),
        project: LinearProjectRef {
            id: "p-1".into(),
            name: "Auth".into(),
            url: "https://linear.app/acme/project/auth".into(),
        },
        operation_id: "op-2".into(),
    });
    assert_eq!(
        serde_json::to_value(&link).unwrap(),
        json!({
            "op": "linkLinearProject", "workItemId": "w-1",
            "project": {"id": "p-1", "name": "Auth", "url": "https://linear.app/acme/project/auth"},
            "operationId": "op-2",
        })
    );
    assert!(link.requires_linear_enabled());

    let toggle = WorkOutlineMutation::SetSpaceLinearEnabled(SetSpaceLinearEnabledParams {
        space_id: "s-1".into(),
        enabled: true,
        operation_id: "op-3".into(),
    });
    assert_eq!(
        serde_json::to_value(&toggle).unwrap(),
        json!({"op": "setSpaceLinearEnabled", "spaceId": "s-1", "enabled": true, "operationId": "op-3"})
    );
    assert!(!toggle.requires_linear_enabled());
    assert_eq!(toggle.operation_id(), "op-3");
}

#[test]
fn snapshot_settings_and_modified_fields_are_additive() {
    // Old frames (no settings, no modified fields) decode with defaults.
    let snapshot: WorkOutlineSnapshot = serde_json::from_value(json!({
        "spaceId": "s-1",
        "items": [{"id": "w-1", "spaceId": "s-1", "title": "T"}],
    }))
    .unwrap();
    assert_eq!(snapshot.space_settings, WorkOutlineSpaceSettings::default());
    assert!(!snapshot.space_settings.linear_enabled);
    let item: &WorkItemView = &snapshot.items[0];
    assert!(item.external_sync_modified_fields.is_empty());
    assert!(item.follows_external_sync(SYNC_FIELD_TITLE));
    // Empty modified fields are omitted; set ones are a string list.
    let value = serde_json::to_value(item).unwrap();
    assert!(value.get("externalSyncModifiedFields").is_none());
    let mut edited = item.clone();
    edited.external_sync_modified_fields = vec![SYNC_FIELD_TITLE.into()];
    let value = serde_json::to_value(&edited).unwrap();
    assert_eq!(value["externalSyncModifiedFields"], json!(["title"]));
    assert!(!edited.follows_external_sync("title"));
    assert!(edited.follows_external_sync("description"));
    let settings = serde_json::to_value(WorkOutlineSnapshot {
        space_settings: WorkOutlineSpaceSettings {
            linear_enabled: true,
        },
        ..Default::default()
    })
    .unwrap();
    assert_eq!(settings["spaceSettings"], json!({"linearEnabled": true}));
}
