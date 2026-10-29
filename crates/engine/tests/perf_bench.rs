//! Engine benchmarks (`#[ignore]`): cold start on an empty and a busy data
//! dir (250 chats, 40 work items with PR + Linear children), and the SWR
//! cache cold-load / persist cost. Run:
//! `cargo test -p mast-engine --profile installed --test perf_bench -- --ignored --nocapture --test-threads=1`

use std::sync::Arc;
use std::time::Instant;

use mast_engine::swr::{Entry, SwrStore};
use mast_engine::{EngineCore, HarnessId, default_registry};
use mast_proto::{
    AssignChatToWorkItemParams, CreateWorkItemExternalReferenceParams, CreateWorkItemParams,
    LinearIssueSummary, WorkOutlineActor, WorkOutlineActorKind, WorkOutlineMutation,
};

fn bench<R>(label: &str, f: impl FnOnce() -> R) -> R {
    let t = Instant::now();
    let out = f();
    eprintln!(
        "BENCH engine | {label} | ms={:.2}",
        t.elapsed().as_secs_f64() * 1000.0
    );
    out
}

fn assemble(dir: &std::path::Path) -> EngineCore {
    EngineCore::assemble(dir, Arc::new(default_registry()), HarnessId::Mock, None).unwrap()
}

fn seed(core: &EngineCore) {
    let ws = &core.workspace;
    let device = "bench-device";
    ws.create_space("space-1", device, "/work", Some("Bench".into()), true)
        .unwrap();
    for i in 0..250 {
        ws.create_chat(
            &format!("c{i}"),
            Some("space-1"),
            None,
            None,
            Some(format!("/work/repo{}", i % 25)),
        )
        .unwrap();
    }
    let actor = WorkOutlineActor {
        kind: WorkOutlineActorKind::User,
        device_id: device.into(),
        chat_id: None,
        message_id: None,
    };
    let mutate = |m: WorkOutlineMutation| ws.work_outline_mutate(&m, &actor, &[]).unwrap();
    let create = |parent: Option<&str>, title: String, op: String| -> String {
        mutate(WorkOutlineMutation::CreateWorkItem(CreateWorkItemParams {
            space_id: "space-1".into(),
            parent_work_item_id: parent.map(Into::into),
            placement: Default::default(),
            title,
            description: None,
            status: None,
            operation_id: op,
        }))
        .work_item_id
        .expect("created item id")
    };
    for i in 0..40 {
        let root = create(None, format!("Ticket {i}"), format!("op-root-{i}"));
        let pr = create(Some(&root), format!("PR review {i}"), format!("op-pr-{i}"));
        let lin = create(Some(&root), format!("ENG-{i}"), format!("op-lin-{i}"));
        for (item, provider, kind, ext) in [
            (&pr, "github", "pullRequest", format!("acme/repo#{i}")),
            (&lin, "linear", "issue", format!("ENG-{i}")),
        ] {
            mutate(WorkOutlineMutation::CreateWorkItemExternalReference(
                CreateWorkItemExternalReferenceParams {
                    work_item_id: item.clone(),
                    provider: provider.into(),
                    reference_type: kind.into(),
                    external_id: ext.clone(),
                    url: format!("https://example.com/{ext}"),
                    cached_title: Some(format!("Title {ext}")),
                    operation_id: format!("op-ref-{item}"),
                },
            ));
        }
        for (n, item) in [&root, &pr, &lin].into_iter().enumerate() {
            mutate(WorkOutlineMutation::AssignChatToWorkItem(
                AssignChatToWorkItemParams {
                    chat_id: format!("c{}", i * 5 + n),
                    work_item_id: item.clone(),
                    operation_id: format!("op-assign-{i}-{n}"),
                },
            ));
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "benchmark: run with --ignored --nocapture"]
async fn bench_engine_startup() {
    let empty = tempfile::tempdir().unwrap();
    let core = bench("assemble, empty data dir", || assemble(empty.path()));
    drop(core);
    bench("assemble, empty data dir (2nd)", || {
        drop(assemble(empty.path()))
    });

    let busy = tempfile::tempdir().unwrap();
    let core = assemble(busy.path());
    bench("seed 250 chats + 40x3 work items", || seed(&core));
    drop(core);
    // Registry snapshots are flushed on a timer; let the writer settle.
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    for n in 1..=3 {
        let core = bench(&format!("assemble, busy data dir (cold {n})"), || {
            assemble(busy.path())
        });
        let chats = bench("read chats + spaces", || {
            core.workspace.read_chats().unwrap().len()
        });
        eprintln!("BENCH engine | chats loaded={chats}");
        drop(core);
    }
}

fn issue(i: usize) -> LinearIssueSummary {
    LinearIssueSummary {
        id: format!("uuid-{i}"),
        identifier: format!("ENG-{i}"),
        title: format!("Issue {i}: something is slow"),
        description: Some("Description paragraph. ".repeat(60)),
        url: format!("https://linear.app/acme/issue/ENG-{i}"),
        updated_at: "2026-10-28T12:00:00Z".into(),
        ..Default::default()
    }
}

#[test]
#[ignore = "benchmark: run with --ignored --nocapture"]
fn bench_swr_cache() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cache.json");
    // 8 keys (tabs/lists) × 300 issues ≈ a heavy Linear + PR cache.
    let entries: std::collections::HashMap<String, Entry<Vec<LinearIssueSummary>>> = (0..8)
        .map(|k| {
            (
                format!("key{k}"),
                Entry {
                    fetched_at: chrono::Utc::now(),
                    value: (0..300).map(|i| issue(k * 1000 + i)).collect(),
                },
            )
        })
        .collect();
    let bytes = serde_json::to_vec(&entries).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    eprintln!("BENCH swr | cache file bytes={}", bytes.len());
    for n in 1..=3 {
        let store: SwrStore<Vec<LinearIssueSummary>, String> =
            SwrStore::new(std::time::Duration::from_secs(60));
        bench(&format!("attach_file cold load ({n})"), || {
            store.attach_file(path.clone())
        });
    }
    // Cost of one `get` that fetches: persists the WHOLE map each time.
    let store: SwrStore<Vec<LinearIssueSummary>, String> =
        SwrStore::new(std::time::Duration::from_secs(60));
    store.attach_file(path.clone());
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    for n in 1..=3 {
        bench(&format!("get(force) + persist whole cache ({n})"), || {
            rt.block_on(store.get("key0", true, || {
                Box::pin(async { Ok((0..300).map(issue).collect()) })
            }))
            .unwrap()
        });
    }
}

/// Cold start on a copy of a real profile (never the live one). Make the copy
/// with `sqlite3 -readonly ~/.zeron/profiles/local/docs.sqlite3 ".backup
/// $DIR/profiles/local/docs.sqlite3"` plus `cp ~/.zeron/local-profile.json
/// $DIR/`, then run with `MAST_PERF_REAL_COPY=$DIR`. Skipped when unset.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "benchmark: needs MAST_PERF_REAL_COPY"]
async fn bench_engine_startup_real_profile_copy() {
    let Some(dir) = std::env::var_os("MAST_PERF_REAL_COPY") else {
        eprintln!("BENCH engine | MAST_PERF_REAL_COPY not set, skipped");
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    for n in 1..=3 {
        let profile = mast_engine::EngineProfile::local(&dir).unwrap();
        let core = bench(&format!("assemble real profile copy (run {n})"), || {
            EngineCore::assemble_with_profile(
                profile,
                Arc::new(default_registry()),
                HarnessId::Mock,
                None,
            )
            .unwrap()
        });
        let chats = bench("read_chats", || core.workspace.read_chats().unwrap());
        eprintln!("BENCH engine | real chats={}", chats.len());
        let t = Instant::now();
        for _ in 0..50 {
            std::hint::black_box(core.workspace.read_chats().unwrap());
        }
        eprintln!(
            "BENCH engine | read_chats steady state x50 | mean_ms={:.2}",
            t.elapsed().as_secs_f64() * 1000.0 / 50.0
        );
        let spaces = core.workspace.read_spaces().unwrap_or_default();
        bench("work_outline_inputs for every space", || {
            for space in &spaces {
                std::hint::black_box(core.workspace.work_outline_inputs(&space.id));
            }
        });
        drop(core);
    }
}

/// Registry publish cost: a live session heartbeat (the commonest registry
/// write while agents run) round trip, from `record_session` to the sessions
/// watch firing, on a copy of a real profile (see above for `MAST_PERF_REAL_COPY`).
#[tokio::test(flavor = "multi_thread")]
#[ignore = "benchmark: needs MAST_PERF_REAL_COPY"]
async fn bench_registry_session_heartbeat_publish() {
    let Some(dir) = std::env::var_os("MAST_PERF_REAL_COPY") else {
        eprintln!("BENCH engine | MAST_PERF_REAL_COPY not set, skipped");
        return;
    };
    let profile = mast_engine::EngineProfile::local(std::path::Path::new(&dir)).unwrap();
    let core = EngineCore::assemble_with_profile(
        profile,
        Arc::new(default_registry()),
        HarnessId::Mock,
        None,
    )
    .unwrap();
    let chat = core.workspace.read_chats().unwrap().remove(0);
    let mut rx = core.workspace.watch_session_rows();
    tokio::time::sleep(std::time::Duration::from_secs(2)).await; // let boot settle
    rx.borrow_and_update();
    let base = chrono::Utc::now();
    let n = 100;
    let t = Instant::now();
    for i in 0..n {
        core.workspace.record_session(&mast_proto::Session {
            last_completed_turn: None,
            chat_id: chat.id.clone(),
            device_id: chat.device_id.clone(),
            status: mast_proto::SessionStatus::Working,
            started_at: None,
            updated_at: base + chrono::TimeDelta::milliseconds(i),
        });
        tokio::time::timeout(std::time::Duration::from_secs(10), rx.changed())
            .await
            .expect("sessions publish")
            .unwrap();
    }
    eprintln!(
        "BENCH engine | session heartbeat -> sessions watch (331 real chats) x{n} | mean_ms={:.2}",
        t.elapsed().as_secs_f64() * 1000.0 / n as f64
    );
}
