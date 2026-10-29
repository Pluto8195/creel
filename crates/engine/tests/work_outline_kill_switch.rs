//! `MAST_DISABLE_WORK_OUTLINE_WRITES` turns every Work Outline mutation into
//! `writesDisabled` while reads keep working. Its own test binary: the env
//! var is process-wide.

use std::sync::Arc;

use mast_engine::{EngineCore, HarnessRegistry};
use mast_proto::{HarnessId, WorkOutlineError, WorkOutlineErrorCode, WorkOutlineSnapshot};
use mast_rpc::methods;
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn kill_switch_refuses_writes_and_keeps_reads() {
    // SAFETY: this binary has exactly one test, so nothing reads the
    // environment concurrently.
    unsafe { std::env::set_var(mast_proto::work_outline::DISABLE_WRITES_ENV, "1") };
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("device-id"), "dev-a").unwrap();
    let core = EngineCore::assemble(
        &data,
        Arc::new(HarnessRegistry::new()),
        HarnessId::Mock,
        None,
    )
    .expect("engine core assembles");
    let client = mast_rpc::memory_client(core.rpc_service());
    client
        .call(
            methods::MUTATE,
            json!({ "op": "createSpace", "spaceId": "space-1", "deviceId": "dev-a", "path": "/tmp" }),
        )
        .await
        .unwrap();
    let err = client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({ "op": "createWorkItem", "spaceId": "space-1", "title": "No", "operationId": "op-1" }),
        )
        .await
        .unwrap_err()
        .to_string();
    let at = err.find("work-outline/").expect("wire-encoded error");
    let parsed = WorkOutlineError::from_wire_message(&err[at..]).unwrap();
    assert_eq!(parsed.code, WorkOutlineErrorCode::WritesDisabled);
    // Instruction operations are covered by the same switch.
    let err = client
        .call(
            methods::WORK_OUTLINE_MUTATE,
            json!({
                "op": "createWorkItemInstruction", "spaceId": "space-1",
                "scope": { "type": "spaceRoot" }, "text": "No", "operationId": "op-2",
            }),
        )
        .await
        .unwrap_err()
        .to_string();
    let at = err.find("work-outline/").expect("wire-encoded error");
    let parsed = WorkOutlineError::from_wire_message(&err[at..]).unwrap();
    assert_eq!(parsed.code, WorkOutlineErrorCode::WritesDisabled);

    let mut stream = client
        .subscribe(methods::WATCH_WORK_OUTLINE, json!({ "spaceId": "space-1" }))
        .await
        .unwrap();
    let frame: WorkOutlineSnapshot =
        serde_json::from_value(stream.recv().await.expect("first frame")).unwrap();
    assert!(frame.available && frame.items.is_empty());
    core.shutdown().await;
}
