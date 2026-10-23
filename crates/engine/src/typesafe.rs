//! Generic TypeSafe (Jev, System One) client pieces: the key lookup, one
//! bounded `POST /v1/systemone` call, and a small circuit breaker.
//!
//! Kept for reuse; no caller since chat classification was removed.
//! `chat_workspace_plan.rs` still carries its own copy of the endpoint/model
//! constants and request code for its needs-worktree judgment.
//!
//! The contract every caller is meant to follow: Jev first, a local heuristic
//! on ANY failure (no key, HTTP error, timeout, bad JSON, torn answer).

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Same endpoint/model/auth convention as `chat_workspace_plan.rs`.
pub const TYPESAFE_API_URL: &str = "https://api.typesafe.ai/v1/systemone";
pub const TYPESAFE_MODEL: &str = "jev-latest";
/// Hard deadline for one whole call (request + parse), so a caller never
/// stalls on a slow or hung TypeSafe response.
pub const JEV_TIMEOUT: Duration = Duration::from_secs(3);

/// `TYPESAFE_API_KEY`, `None` when unset or empty.
pub fn api_key() -> Option<String> {
    std::env::var("TYPESAFE_API_KEY")
        .ok()
        .filter(|s| !s.is_empty())
}

/// POST `payload` to a System One endpoint and return the parsed JSON body.
/// `None` on ANY failure: network error, non-success status, bad JSON, or
/// `timeout` elapsing. Endpoint/key/timeout are parameters so tests can point
/// it at a local server.
pub async fn post_system_one(
    http: &reqwest::Client,
    url: &str,
    api_key: &str,
    timeout: Duration,
    payload: &serde_json::Value,
) -> Option<serde_json::Value> {
    let call = async {
        let response = http
            .post(url)
            .bearer_auth(api_key)
            .json(payload)
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        response.json::<serde_json::Value>().await.ok()
    };
    tokio::time::timeout(timeout, call).await.ok().flatten()
}

/// Consecutive failures before the breaker opens.
const BREAKER_THRESHOLD: u32 = 3;
/// How long an open breaker short-circuits calls.
const BREAKER_COOLDOWN: Duration = Duration::from_secs(5 * 60);

#[derive(Default)]
struct BreakerState {
    consecutive_failures: u32,
    open_until: Option<Instant>,
}

/// Keeps a hung or down TypeSafe from costing the full timeout on every call
/// of a batch: after [`BREAKER_THRESHOLD`] consecutive failures,
/// [`Self::is_open`] stays true for [`BREAKER_COOLDOWN`], then one probe is
/// allowed through (half-open).
#[derive(Default)]
pub struct CircuitBreaker {
    state: Mutex<BreakerState>,
}

impl CircuitBreaker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether calls should currently be skipped.
    pub fn is_open(&self) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        match state.open_until {
            Some(until) if Instant::now() < until => true,
            Some(_) => {
                // Cooldown elapsed: half-open, allow probing again.
                state.open_until = None;
                state.consecutive_failures = BREAKER_THRESHOLD - 1;
                false
            }
            None => false,
        }
    }

    /// Record one call's outcome: `failed` for a transport/service failure,
    /// `false` for any answer the service actually gave.
    pub fn record(&self, failed: bool) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if failed {
            state.consecutive_failures += 1;
            if state.consecutive_failures >= BREAKER_THRESHOLD {
                state.open_until = Some(Instant::now() + BREAKER_COOLDOWN);
            }
        } else {
            state.consecutive_failures = 0;
            state.open_until = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// One-shot local HTTP server: answers the first request with `status`
    /// and `body` (or never answers when `hang`), returns its URL.
    async fn serve_once(status: u16, body: &'static str, hang: bool) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1/systemone", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 65536];
            let _ = stream.read(&mut buf).await;
            if hang {
                tokio::time::sleep(Duration::from_secs(30)).await;
                return;
            }
            let reply = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(reply.as_bytes()).await;
        });
        url
    }

    const GOOD_BODY: &str = r#"{"model":"m","answers":{"q":{"type":"choice","probabilities":{"a":0.9,"b":0.1}}},"usage":{}}"#;

    async fn run(url: &str, timeout: Duration) -> Option<serde_json::Value> {
        post_system_one(
            &reqwest::Client::new(),
            url,
            "test-key",
            timeout,
            &serde_json::json!({"model": TYPESAFE_MODEL, "state": {}}),
        )
        .await
    }

    #[tokio::test]
    async fn http_success_yields_the_parsed_body() {
        let url = serve_once(200, GOOD_BODY, false).await;
        let body = run(&url, Duration::from_secs(5)).await.unwrap();
        assert_eq!(body["answers"]["q"]["probabilities"]["a"], 0.9);
    }

    #[tokio::test]
    async fn http_error_status_is_none() {
        let url = serve_once(500, GOOD_BODY, false).await;
        assert!(run(&url, Duration::from_secs(5)).await.is_none());
        let url = serve_once(401, r#"{"error":"nope"}"#, false).await;
        assert!(run(&url, Duration::from_secs(5)).await.is_none());
    }

    #[tokio::test]
    async fn bad_json_is_none() {
        let url = serve_once(200, "not json at all", false).await;
        assert!(run(&url, Duration::from_secs(5)).await.is_none());
    }

    #[tokio::test]
    async fn timeout_is_none() {
        let url = serve_once(200, GOOD_BODY, true).await;
        let started = Instant::now();
        assert!(run(&url, Duration::from_millis(150)).await.is_none());
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn connection_refused_is_none() {
        // Bind then drop to get a port nothing listens on.
        let port = TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let url = format!("http://127.0.0.1:{port}/v1/systemone");
        assert!(run(&url, Duration::from_secs(5)).await.is_none());
    }

    #[test]
    fn breaker_opens_after_consecutive_failures_and_closes_on_success() {
        let breaker = CircuitBreaker::new();
        assert!(!breaker.is_open());
        for _ in 0..BREAKER_THRESHOLD {
            breaker.record(true);
        }
        assert!(breaker.is_open());

        let breaker = CircuitBreaker::new();
        breaker.record(true);
        breaker.record(true);
        breaker.record(false); // service answered: resets
        breaker.record(true);
        assert!(!breaker.is_open());
    }
}
