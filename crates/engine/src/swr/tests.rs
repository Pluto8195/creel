use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures::FutureExt;

use super::*;

type Store = SwrStore<Vec<u32>, String>;

fn counting(calls: &Arc<AtomicUsize>, value: Vec<u32>) -> impl FnOnce() -> BoxFuture<'static, Result<Vec<u32>, String>> {
    let calls = calls.clone();
    move || {
        async move {
            calls.fetch_add(1, Ordering::SeqCst);
            for _ in 0..5 {
                tokio::task::yield_now().await;
            }
            Ok(value)
        }
        .boxed()
    }
}

#[tokio::test]
async fn fresh_value_makes_no_request() {
    let store = Store::new(Duration::from_secs(60));
    let calls = Arc::new(AtomicUsize::new(0));
    store.get("k", false, counting(&calls, vec![1])).await.unwrap();
    let again = store.get("k", false, counting(&calls, vec![2])).await.unwrap();
    assert_eq!(again.value, vec![1]);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn force_bypasses_the_window() {
    let store = Store::new(Duration::from_secs(60));
    let calls = Arc::new(AtomicUsize::new(0));
    store.get("k", false, counting(&calls, vec![1])).await.unwrap();
    let forced = store.get("k", true, counting(&calls, vec![2])).await.unwrap();
    assert_eq!(forced.value, vec![2]);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn concurrent_gets_share_one_request() {
    let store = Store::new(Duration::from_secs(60));
    let calls = Arc::new(AtomicUsize::new(0));
    let (a, b, c) = tokio::join!(
        store.get("k", false, counting(&calls, vec![1])),
        store.get("k", false, counting(&calls, vec![1])),
        store.get("k", true, counting(&calls, vec![1])),
    );
    assert!(a.is_ok() && b.is_ok() && c.is_ok());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn failure_keeps_the_stale_value() {
    let store = Store::new(Duration::ZERO);
    let calls = Arc::new(AtomicUsize::new(0));
    store.get("k", false, counting(&calls, vec![1])).await.unwrap();
    let failed = store
        .get("k", false, || async { Err("boom".to_string()) }.boxed())
        .await;
    assert_eq!(failed.unwrap_err(), "boom");
    assert_eq!(store.peek("k").unwrap().value, vec![1]);
}

#[tokio::test]
async fn a_cold_start_loads_the_persisted_cache() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cache.json");
    let first = Store::new(Duration::from_secs(60));
    first.attach_file(path.clone());
    let calls = Arc::new(AtomicUsize::new(0));
    first.get("k", false, counting(&calls, vec![7])).await.unwrap();
    // The write runs on a blocking worker.
    for _ in 0..100 {
        if path.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let second = Store::new(Duration::from_secs(60));
    second.attach_file(path);
    assert_eq!(second.peek("k").unwrap().value, vec![7]);
    // Still inside the window: no request.
    let hit = second.get("k", false, counting(&calls, vec![9])).await.unwrap();
    assert_eq!(hit.value, vec![7]);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn stale_persisted_value_is_peekable_then_refetched() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cache.json");
    let old = Utc::now() - chrono::Duration::minutes(10);
    std::fs::write(
        &path,
        serde_json::json!({ "k": { "fetchedAt": old, "value": [1] } }).to_string(),
    )
    .unwrap();
    let store = Store::new(Duration::from_secs(60));
    store.attach_file(path);
    assert_eq!(store.peek("k").unwrap().value, vec![1]);
    let calls = Arc::new(AtomicUsize::new(0));
    let fresh = store.get("k", false, counting(&calls, vec![2])).await.unwrap();
    assert_eq!(fresh.value, vec![2]);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
