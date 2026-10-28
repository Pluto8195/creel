//! Wire-shape tests for the PR review sync contract: JSON field names,
//! leniency, error strings, titles and keys, and the Work Outline additions.

use serde_json::json;

use super::*;
use crate::work_outline::{
    SetPrReviewSyncDestinationParams, WorkItemChanges, WorkOutlineMutation, WorkOutlineSnapshot,
    WorkOutlineSpaceSettings,
};
use crate::work_outline_user_actions::{
    UserActionOriginKind, UserActionRequestOrigin, UserActionRequestView,
};

fn round_trip<T>(value: &T, expected: serde_json::Value)
where
    T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    assert_eq!(serde_json::to_value(value).unwrap(), expected);
    assert_eq!(&serde_json::from_value::<T>(expected).unwrap(), value);
}

fn summary() -> PullRequestStatusSummary {
    PullRequestStatusSummary {
        pr_ref: PullRequestRef::new("Cofactr", "flagship", 4321),
        title: "Fix login".into(),
        author: "ada".into(),
        state: PullRequestState::Draft,
        ci_status: PullRequestCiStatus::Failing,
        review_decision: Some(PullRequestReviewDecision::ChangesRequested),
        my_review_state: PullRequestMyReviewState::ReRequested,
        requested_reviewers: vec!["mikey".into(), "Cofactr/eng".into()],
        updated_at: "2026-10-14T10:00:00Z".into(),
        head_ref_name: Some("ada/fix-login".into()),
        base_ref_name: Some("main".into()),
    }
}

#[test]
fn status_summary_field_names_are_pinned() {
    round_trip(
        &summary(),
        json!({
            "ref": {
                "owner": "Cofactr", "repo": "flagship", "number": 4321,
                "url": "https://github.com/Cofactr/flagship/pull/4321"
            },
            "title": "Fix login",
            "author": "ada",
            "state": "draft",
            "ciStatus": "failing",
            "reviewDecision": "changesRequested",
            "myReviewState": "reRequested",
            "requestedReviewers": ["mikey", "Cofactr/eng"],
            "updatedAt": "2026-10-14T10:00:00Z",
            "headRefName": "ada/fix-login",
            "baseRefName": "main"
        }),
    );
}

#[test]
fn enums_are_lenient_and_fields_default() {
    let decoded: PullRequestStatusSummary = serde_json::from_value(json!({
        "ref": {"owner": "o", "repo": "r", "number": 1},
        "state": "queued",
        "ciStatus": "exploded",
        "reviewDecision": "SOMETHING",
        "myReviewState": "dismissed"
    }))
    .unwrap();
    assert_eq!(decoded.state, PullRequestState::Open);
    assert_eq!(decoded.ci_status, PullRequestCiStatus::None);
    assert_eq!(
        decoded.review_decision,
        Some(PullRequestReviewDecision::ReviewRequired)
    );
    assert_eq!(decoded.my_review_state, PullRequestMyReviewState::None);
    assert!(decoded.requested_reviewers.is_empty());
    assert_eq!(decoded.head_ref_name, None);
    for (text, state) in [
        ("open", PullRequestState::Open),
        ("draft", PullRequestState::Draft),
        ("merged", PullRequestState::Merged),
        ("closed", PullRequestState::Closed),
    ] {
        assert_eq!(serde_json::to_value(state).unwrap(), json!(text));
    }
    for (text, state) in [
        ("requested", PullRequestMyReviewState::Requested),
        ("reRequested", PullRequestMyReviewState::ReRequested),
        ("commented", PullRequestMyReviewState::Commented),
        ("approved", PullRequestMyReviewState::Approved),
        ("changesRequested", PullRequestMyReviewState::ChangesRequested),
        ("none", PullRequestMyReviewState::None),
    ] {
        assert_eq!(serde_json::to_value(state).unwrap(), json!(text));
    }
    for (text, state) in [
        ("passing", PullRequestCiStatus::Passing),
        ("failing", PullRequestCiStatus::Failing),
        ("pending", PullRequestCiStatus::Pending),
        ("none", PullRequestCiStatus::None),
    ] {
        assert_eq!(serde_json::to_value(state).unwrap(), json!(text));
    }
}

#[test]
fn titles_keys_and_external_ids() {
    assert_eq!(
        pr_item_title("flagship", 12, "  Fix   the\nlogin "),
        "flagship#12 · Fix the login"
    );
    assert_eq!(pr_item_title("ui", 3, ""), "ui#3");
    let long = "x".repeat(400);
    assert_eq!(
        pr_item_title("ui", 3, &long).chars().count(),
        crate::work_outline::limits::MAX_TITLE_CHARS
    );
    assert_eq!(pr_external_id("Cofactr", "ui", 9), "Cofactr/ui#9");
    assert_eq!(pr_key("Cofactr", "UI", 9), "cofactr/ui#9");
    assert_eq!(
        pr_review_dedupe_key("Cofactr", "ui", 9, 2),
        "pr-review:cofactr/ui#9:2"
    );
    let parsed = PullRequestRef::parse_external_id("Cofactr/ui#9").unwrap();
    assert_eq!(parsed, PullRequestRef::new("Cofactr", "ui", 9));
    assert_eq!(parsed.url, "https://github.com/Cofactr/ui/pull/9");
    assert_eq!(parsed.external_id(), "Cofactr/ui#9");
    for bad in ["", "ui#9", "Cofactr/ui#0", "Cofactr/ui#x", "a/b/c#1", "/ui#1"] {
        assert_eq!(PullRequestRef::parse_external_id(bad), None, "{bad}");
    }
}

#[test]
fn errors_round_trip_through_the_wire_string() {
    for code in [
        GithubErrorCode::GhMissing,
        GithubErrorCode::Unauthorized,
        GithubErrorCode::RateLimited,
        GithubErrorCode::NotFound,
        GithubErrorCode::Network,
        GithubErrorCode::Invalid,
    ] {
        let error = GithubError::new(code, "boom: detail");
        let wire = error.to_wire_message();
        assert_eq!(wire, format!("github/{}: boom: detail", code.as_str()));
        assert_eq!(
            GithubError::from_wire_message(&format!("rpc failed: {wire}")),
            Some(error)
        );
        assert_eq!(serde_json::to_value(code).unwrap(), json!(code.as_str()));
    }
    assert_eq!(
        GithubError::from_wire_message("github/whatever: x").unwrap().code,
        GithubErrorCode::Invalid
    );
    assert_eq!(GithubError::from_wire_message("linear/network: x"), None);
}

#[test]
fn rpc_params_and_replies_are_pinned() {
    round_trip(
        &PrStatusParams {
            owner: "o".into(),
            repo: "r".into(),
            number: 5,
            force_refresh: true,
        },
        json!({"owner": "o", "repo": "r", "number": 5, "forceRefresh": true}),
    );
    round_trip(
        &PrStatusParams {
            owner: "o".into(),
            repo: "r".into(),
            number: 5,
            force_refresh: false,
        },
        json!({"owner": "o", "repo": "r", "number": 5}),
    );
    round_trip(
        &PrReviewSyncNowParams {
            space_id: "s".into(),
        },
        json!({"spaceId": "s"}),
    );
    round_trip(
        &PrReviewSyncNowReply {
            created: 2,
            updated: 1,
            resolved: 3,
        },
        json!({"created": 2, "updated": 1, "resolved": 3}),
    );
    round_trip(
        &PrMyOpenParams {
            space_id: "s".into(),
        },
        json!({"spaceId": "s"}),
    );
    round_trip(
        &PrReviewRequestedParams {
            space_id: "s".into(),
        },
        json!({"spaceId": "s"}),
    );
}

#[test]
fn sync_state_and_work_outline_additions_are_pinned() {
    let state = WorkItemExternalSyncState {
        provider: GITHUB_PROVIDER.into(),
        external_id: "Cofactr/ui#9".into(),
        round: 2,
        request_state: PrReviewRequestState::Submitted,
        synced_status: crate::WorkItemStatus::Completed,
        round_started_at: 100,
        synced_at: 200,
        archived_by_sync: None,
        marker: None,
        archived_by_linear_sync: false,
    };
    round_trip(
        &state,
        json!({
            "provider": "github", "externalId": "Cofactr/ui#9", "round": 2,
            "requestState": "submitted", "syncedStatus": "completed",
            "roundStartedAt": 100, "syncedAt": 200
        }),
    );
    let lenient: WorkItemExternalSyncState =
        serde_json::from_value(json!({"requestState": "later"})).unwrap();
    assert_eq!(lenient.request_state, PrReviewRequestState::Requested);
    for (text, value) in [
        ("requested", PrReviewRequestState::Requested),
        ("submitted", PrReviewRequestState::Submitted),
        ("withdrawn", PrReviewRequestState::Withdrawn),
        ("merged", PrReviewRequestState::Merged),
        ("closed", PrReviewRequestState::Closed),
    ] {
        assert_eq!(serde_json::to_value(value).unwrap(), json!(text));
    }

    round_trip(
        &WorkOutlineMutation::SetPrReviewSyncDestination(SetPrReviewSyncDestinationParams {
            space_id: "s".into(),
            work_item_id: Some("w".into()),
            operation_id: "op".into(),
        }),
        json!({"op": "setPrReviewSyncDestination", "spaceId": "s", "workItemId": "w",
               "operationId": "op"}),
    );
    let clear: WorkOutlineMutation = serde_json::from_value(
        json!({"op": "setPrReviewSyncDestination", "spaceId": "s", "workItemId": null,
               "operationId": "op"}),
    )
    .unwrap();
    assert_eq!(clear.op_name(), "setPrReviewSyncDestination");
    assert_eq!(clear.operation_id(), "op");

    let settings = serde_json::to_value(WorkOutlineSnapshot {
        space_settings: WorkOutlineSpaceSettings {
            linear_enabled: false,
            pr_review_sync_work_item_id: Some("w".into()),
        },
        ..Default::default()
    })
    .unwrap();
    assert_eq!(
        settings["spaceSettings"],
        json!({"linearEnabled": false, "prReviewSyncWorkItemId": "w"})
    );
    let defaults = serde_json::to_value(WorkOutlineSpaceSettings::default()).unwrap();
    assert_eq!(defaults, json!({"linearEnabled": false}));

    let changes = WorkItemChanges {
        external_sync_state: Some(state.clone()),
        ..Default::default()
    };
    assert_eq!(
        serde_json::to_value(&changes).unwrap()["externalSyncState"]["round"],
        json!(2)
    );
    assert_eq!(
        serde_json::to_value(WorkItemChanges::default()).unwrap(),
        json!({})
    );
}

#[test]
fn system_origin_requests_are_pinned() {
    let origin = UserActionRequestOrigin {
        kind: UserActionOriginKind::System,
        chat_id: String::new(),
        message_id: None,
        tool_call_id: None,
    };
    round_trip(&origin, json!({"kind": "system", "chatId": ""}));
    // A chat origin keeps its old shape; `kind` is omitted and defaults.
    let chat: UserActionRequestOrigin = serde_json::from_value(json!({"chatId": "c"})).unwrap();
    assert_eq!(chat.kind, UserActionOriginKind::Chat);
    assert_eq!(serde_json::to_value(&chat).unwrap(), json!({"chatId": "c"}));
    // A system origin may omit chatId.
    let bare: UserActionRequestOrigin = serde_json::from_value(json!({"kind": "system"})).unwrap();
    assert_eq!(bare, origin);
    // Unknown kinds read as chat.
    let odd: UserActionRequestOrigin =
        serde_json::from_value(json!({"kind": "robot", "chatId": "c"})).unwrap();
    assert_eq!(odd.kind, UserActionOriginKind::Chat);

    let view: UserActionRequestView = serde_json::from_value(json!({
        "id": "r", "originKind": "system", "text": "Review requested: ui#9 Fix"
    }))
    .unwrap();
    assert_eq!(view.origin_kind, UserActionOriginKind::System);
    assert!(!view.origin_chat_available);
    assert_eq!(serde_json::to_value(&view).unwrap()["originKind"], json!("system"));
}

#[test]
fn capability_is_advertised() {
    assert_eq!(PR_REVIEW_SYNC_CAPABILITY_V1, "pr-review-sync-v1");
    assert!(crate::capabilities::current().contains(&"pr-review-sync-v1".to_owned()));
}

#[test]
fn archived_by_sync_marker_is_optional_and_round_trips() {
    let mut state = WorkItemExternalSyncState {
        request_state: PrReviewRequestState::Merged,
        ..Default::default()
    };
    assert!(serde_json::to_value(&state).unwrap().get("archivedBySync").is_none());
    state.archived_by_sync = Some(PrReviewRequestState::Merged);
    let value = serde_json::to_value(&state).unwrap();
    assert_eq!(value["archivedBySync"], "merged");
    assert_eq!(serde_json::from_value::<WorkItemExternalSyncState>(value).unwrap(), state);
}
