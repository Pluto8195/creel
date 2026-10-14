//! One GraphQL round trip to Linear: bare `Authorization: <key>` (Linear
//! rejects `Bearer` for personal keys), a request timeout, HTTP 429 (or a
//! `RATELIMITED` GraphQL error) retried after `Retry-After` capped at 30 s
//! with a bounded number of attempts, and Linear's own error messages
//! surfaced. The key is never put in an error or a log line.

use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use serde_json::{Value, json};
use zeron_proto::linear::{LinearError, LinearErrorCode};

pub const LINEAR_GRAPHQL_URL: &str = "https://api.linear.app/graphql";
/// Linear's rate-limit window is long; never wait more than this per retry.
pub const MAX_RETRY_AFTER: Duration = Duration::from_secs(30);
/// Wait when a 429 carries no usable `Retry-After`.
const DEFAULT_RETRY_AFTER: Duration = Duration::from_secs(5);
/// Total attempts of one request when rate-limited.
pub const MAX_ATTEMPTS: usize = 3;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// How the client waits between rate-limit retries (tests record instead).
pub type Sleeper = Arc<dyn Fn(Duration) -> BoxFuture<'static, ()> + Send + Sync>;

pub fn tokio_sleeper() -> Sleeper {
    Arc::new(|duration| Box::pin(tokio::time::sleep(duration)))
}

#[derive(Clone)]
pub struct LinearClient {
    http: reqwest::Client,
    endpoint: String,
    sleeper: Sleeper,
}

impl LinearClient {
    pub fn new(endpoint: impl Into<String>, sleeper: Sleeper) -> Self {
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            http,
            endpoint: endpoint.into(),
            sleeper,
        }
    }

    /// POST one GraphQL document; the response's `data` on success.
    pub async fn graphql(
        &self,
        key: &str,
        query: &str,
        variables: Value,
    ) -> Result<Value, LinearError> {
        let body = json!({ "query": query, "variables": variables });
        let mut attempt = 0;
        loop {
            attempt += 1;
            match self.attempt(key, &body).await {
                Attempt::Done(result) => return result,
                Attempt::RateLimited(wait) => {
                    if attempt >= MAX_ATTEMPTS {
                        return Err(LinearError::new(
                            LinearErrorCode::RateLimited,
                            "Linear is rate-limiting requests; try again later",
                        ));
                    }
                    tracing::info!(
                        wait_ms = wait.as_millis() as u64,
                        attempt,
                        "linear: rate-limited, retrying"
                    );
                    (self.sleeper)(wait).await;
                }
            }
        }
    }

    async fn attempt(&self, key: &str, body: &Value) -> Attempt {
        let response = match self
            .http
            .post(&self.endpoint)
            .header(reqwest::header::AUTHORIZATION, key)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(body)
            .send()
            .await
        {
            Ok(response) => response,
            Err(err) => {
                let what = if err.is_timeout() {
                    "Linear did not answer in time".to_owned()
                } else {
                    // `reqwest` errors name the URL, never headers.
                    format!("Could not reach Linear: {}", without_url(&err))
                };
                return Attempt::Done(Err(LinearError::new(LinearErrorCode::Network, what)));
            }
        };
        let status = response.status();
        let retry_after = retry_after(response.headers());
        let text = response.text().await.unwrap_or_default();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Attempt::Done(Err(unauthorized()));
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Attempt::RateLimited(retry_after);
        }
        let payload: Value = match serde_json::from_str(&text) {
            Ok(payload) => payload,
            Err(_) => {
                return Attempt::Done(Err(LinearError::new(
                    LinearErrorCode::Network,
                    format!(
                        "Linear answered HTTP {} with an unreadable body",
                        status.as_u16()
                    ),
                )));
            }
        };
        let errors: Vec<&Value> = payload
            .get("errors")
            .and_then(Value::as_array)
            .map(|e| e.iter().collect())
            .unwrap_or_default();
        if !errors.is_empty() {
            let codes: Vec<String> = errors
                .iter()
                .filter_map(|e| e.pointer("/extensions/code").and_then(Value::as_str))
                .map(str::to_ascii_uppercase)
                .collect();
            let message = errors
                .iter()
                .filter_map(|e| e.get("message").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("; ");
            if codes.iter().any(|c| c == "RATELIMITED") {
                return Attempt::RateLimited(retry_after);
            }
            let lower = message.to_ascii_lowercase();
            if codes.iter().any(|c| c.contains("AUTHENTICATION"))
                || lower.contains("authentication required")
                || lower.contains("invalid api key")
            {
                return Attempt::Done(Err(unauthorized()));
            }
            if lower.contains("not found") || codes.iter().any(|c| c.contains("NOT_FOUND")) {
                return Attempt::Done(Err(LinearError::new(LinearErrorCode::NotFound, message)));
            }
            let code = if status.is_server_error() {
                LinearErrorCode::Network
            } else {
                LinearErrorCode::Invalid
            };
            return Attempt::Done(Err(LinearError::new(
                code,
                format!("Linear: {}", truncate(&message, 500)),
            )));
        }
        if !status.is_success() {
            return Attempt::Done(Err(LinearError::new(
                LinearErrorCode::Network,
                format!("Linear answered HTTP {}", status.as_u16()),
            )));
        }
        Attempt::Done(Ok(payload.get("data").cloned().unwrap_or(Value::Null)))
    }
}

enum Attempt {
    Done(Result<Value, LinearError>),
    RateLimited(Duration),
}

fn unauthorized() -> LinearError {
    LinearError::new(
        LinearErrorCode::Unauthorized,
        "Linear rejected the API key; enter a new one",
    )
}

/// `Retry-After` in seconds (an HTTP date or garbage reads as the default),
/// capped at [`MAX_RETRY_AFTER`].
pub fn retry_after(headers: &reqwest::header::HeaderMap) -> Duration {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|s| s.is_finite() && *s >= 0.0)
        .map_or(DEFAULT_RETRY_AFTER, Duration::from_secs_f64)
        .min(MAX_RETRY_AFTER)
}

fn without_url(err: &reqwest::Error) -> String {
    let mut text = err.to_string();
    if let Some(url) = err.url() {
        text = text.replace(url.as_str(), "Linear");
    }
    text
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        let mut out: String = text.chars().take(max).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_is_capped_and_defaulted() {
        let mut headers = reqwest::header::HeaderMap::new();
        assert_eq!(retry_after(&headers), DEFAULT_RETRY_AFTER);
        headers.insert(reqwest::header::RETRY_AFTER, "2".parse().unwrap());
        assert_eq!(retry_after(&headers), Duration::from_secs(2));
        headers.insert(reqwest::header::RETRY_AFTER, "3600".parse().unwrap());
        assert_eq!(retry_after(&headers), MAX_RETRY_AFTER);
        headers.insert(
            reqwest::header::RETRY_AFTER,
            "Wed, 21 Oct 2026 07:28:00 GMT".parse().unwrap(),
        );
        assert_eq!(retry_after(&headers), DEFAULT_RETRY_AFTER);
    }
}
