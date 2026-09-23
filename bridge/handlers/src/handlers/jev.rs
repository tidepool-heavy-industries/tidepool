// ============================================================================
// Jev / System One HTTP client
// ============================================================================
//
// Production consumer for TypeSafe's Jev API. Mirrors the transport shape
// worked out in `jev-integration/src/transport.rs` (no redirects, no
// retries, streamed body with a hard cap, sensitive bearer header) but
// exposes a typed `JevFailure` and folds key resolution + call budgeting in,
// since this is the handler Exomonad actually dispatches against.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use reqwest::header::{HeaderValue, AUTHORIZATION};

/// Response bytes are streamed and capped here — same limit as the research
/// transport.
const MAX_BODY: usize = 2 * 1024 * 1024;

/// Bytes of a non-2xx body kept for `JevFailure::Http`.
const MAX_ERROR_BODY: usize = 2000;

pub struct JevConfig {
    pub base_url: String,
    pub timeout: Duration,
    pub max_calls: u64,
}

impl Default for JevConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.typesafe.ai".to_string(),
            timeout: Duration::from_secs(15),
            max_calls: 100_000,
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum JevFailure {
    #[error("Jev is not configured: set TYPESAFE_API_KEY or provide ~/.config/typesafe/api-key")]
    Unconfigured,
    #[error("Jev call budget exhausted")]
    CallCap,
    #[error("Jev transport error: {0}")]
    Transport(String),
    #[error("Jev call timed out")]
    Timeout,
    #[error("Jev returned HTTP {status}: {body}")]
    Http { status: u16, body: String },
    #[error("Jev response exceeded the {MAX_BODY}-byte cap")]
    BodyLimit,
    #[error("Jev response was not valid JSON: {0}")]
    Malformed(String),
}

/// Resolves `TYPESAFE_API_KEY`: process env first (non-empty), else
/// `secrets_dir()/TYPESAFE_API_KEY`, then `~/.config/typesafe/api-key`
/// (trimmed, non-empty), else `None`. Never
/// writes to the process environment.
fn resolve_key() -> Option<String> {
    if let Ok(key) = std::env::var("TYPESAFE_API_KEY") {
        if !key.trim().is_empty() {
            return Some(key);
        }
    }
    let primary = tidepool_toolchain::paths::secrets_dir().join("TYPESAFE_API_KEY");
    let fallback = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .map(|home| home.join(".config/typesafe/api-key"));
    read_key_files(std::iter::once(primary).chain(fallback))
}

fn read_key_files(paths: impl IntoIterator<Item = std::path::PathBuf>) -> Option<String> {
    paths.into_iter().find_map(|path| {
        let contents = std::fs::read_to_string(path).ok()?;
        let trimmed = contents.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

fn bearer(key: &str) -> HeaderValue {
    // Key is resolved non-empty and callers never pass control characters in
    // practice, but if header construction ever failed we still must not
    // leak the key into an error string — fall back to an inert value that
    // the server will reject with 401 rather than panicking or logging it.
    let mut value = HeaderValue::from_str(&format!("Bearer {key}"))
        .unwrap_or_else(|_| HeaderValue::from_static("Bearer invalid"));
    value.set_sensitive(true);
    value
}

/// Redacts every occurrence of `key` in `text`, if `key` is non-empty.
fn redact(text: &str, key: Option<&str>) -> String {
    match key {
        Some(k) if !k.is_empty() => text.replace(k, "[redacted]"),
        _ => text.to_string(),
    }
}

pub struct JevClient {
    http: reqwest::Client,
    key: Option<String>,
    config: JevConfig,
    calls: AtomicU64,
}

impl JevClient {
    /// Resolves the key from env/secrets. Building the underlying
    /// `reqwest::Client` is infallible for this configuration, but errors
    /// are threaded through as `JevFailure` for symmetry with `ask`.
    pub fn new(config: JevConfig) -> Result<Self, JevFailure> {
        Self::with_key(config, resolve_key())
    }

    /// For tests: bypass env/secrets resolution and supply the key directly.
    pub fn with_key(config: JevConfig, key: Option<String>) -> Result<Self, JevFailure> {
        let http = reqwest::Client::builder()
            .timeout(config.timeout)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .map_err(|e| JevFailure::Transport(e.to_string()))?;
        Ok(Self {
            http,
            key,
            config,
            calls: AtomicU64::new(0),
        })
    }

    pub fn configured(&self) -> bool {
        self.key.is_some()
    }

    pub async fn ask(&self, body: serde_json::Value) -> Result<serde_json::Value, JevFailure> {
        let Some(key) = self.key.as_deref() else {
            return Err(JevFailure::Unconfigured);
        };
        let count = self.calls.fetch_add(1, Ordering::Relaxed) + 1;
        if count > self.config.max_calls {
            return Err(JevFailure::CallCap);
        }

        let start = std::time::Instant::now();
        let url = format!("{}/v1/systemone", self.config.base_url);
        let send = self
            .http
            .post(&url)
            .header(AUTHORIZATION, bearer(key))
            .json(&body)
            .send();

        let response = match tokio::time::timeout(self.config.timeout, send).await {
            Err(_) => return Err(JevFailure::Timeout),
            Ok(Err(e)) => {
                return Err(if e.is_timeout() {
                    JevFailure::Timeout
                } else {
                    JevFailure::Transport(redact(&e.to_string(), Some(key)))
                })
            }
            Ok(Ok(response)) => response,
        };
        let status = response.status();

        // Stream the body under the whole-exchange timeout, capped at
        // MAX_BODY — mirrors jev-integration's transport.
        let read_body = async {
            let mut bytes = Vec::new();
            let mut stream = response;
            loop {
                match stream.chunk().await {
                    Ok(Some(chunk)) => {
                        let remaining = MAX_BODY - bytes.len();
                        if chunk.len() > remaining {
                            return Err(JevFailure::BodyLimit);
                        }
                        bytes.extend_from_slice(&chunk);
                    }
                    Ok(None) => return Ok(bytes),
                    Err(e) => {
                        return Err(if e.is_timeout() {
                            JevFailure::Timeout
                        } else {
                            JevFailure::Transport(redact(&e.to_string(), Some(key)))
                        })
                    }
                }
            }
        };
        let remaining = self
            .config
            .timeout
            .saturating_sub(start.elapsed())
            // A zero remaining budget would make `tokio::time::timeout` fire
            // immediately even on an already-buffered body; give the body
            // read a last sliver so a fast small response still succeeds.
            .max(Duration::from_millis(1));
        let bytes = match tokio::time::timeout(remaining, read_body).await {
            Err(_) => return Err(JevFailure::Timeout),
            Ok(result) => result?,
        };

        if !status.is_success() {
            let text = String::from_utf8_lossy(&bytes);
            let truncated: String = text.chars().take(MAX_ERROR_BODY).collect();
            return Err(JevFailure::Http {
                status: status.as_u16(),
                body: redact(&truncated, Some(key)),
            });
        }

        let parsed: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|e| JevFailure::Malformed(e.to_string()))?;

        let resolved = parsed
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let usage = parsed
            .get("usage")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        tracing::info!(
            model = %resolved,
            status = status.as_u16(),
            elapsed_ms = start.elapsed().as_millis() as u64,
            usage = %usage,
            "jev call"
        );

        Ok(parsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn key_files_fall_back_and_preserve_priority() {
        let directory = tempfile::tempdir().unwrap();
        let primary = directory.path().join("primary");
        let fallback = directory.path().join("fallback");
        std::fs::write(&fallback, " fallback-test-key\n").unwrap();
        let resolve = || read_key_files([primary.clone(), fallback.clone()]);
        assert_eq!(resolve().as_deref(), Some("fallback-test-key"));
        std::fs::write(&primary, " \n").unwrap();
        assert_eq!(resolve().as_deref(), Some("fallback-test-key"));
        std::fs::write(&primary, "primary-test-key\n").unwrap();
        assert_eq!(resolve().as_deref(), Some("primary-test-key"));
    }

    /// Serves one canned HTTP/1.1 response on a local ephemeral port and
    /// returns (url, join-handle capturing the raw request bytes received).
    #[allow(
        clippy::disallowed_methods,
        reason = "short synchronous delay on a dedicated test-fixture thread simulating a \
                  slow server reply, not a long-lived child needing the launcher"
    )]
    fn server(status: &str, body: &str, delay: Duration) -> (String, thread::JoinHandle<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let reply = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let task = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut received = Vec::new();
            let mut buffer = [0; 4096];
            while !received.windows(4).any(|w| w == b"\r\n\r\n") {
                let count = stream.read(&mut buffer).unwrap();
                if count == 0 {
                    break;
                }
                received.extend_from_slice(&buffer[..count]);
            }
            thread::sleep(delay);
            stream.write_all(reply.as_bytes()).ok();
            received
        });
        (url, task)
    }

    fn config(base_url: String) -> JevConfig {
        JevConfig {
            base_url,
            timeout: Duration::from_secs(2),
            max_calls: 100_000,
        }
    }

    #[tokio::test]
    async fn success_round_trip_sends_bearer_and_path() {
        let (url, task) = server(
            "200 OK",
            r#"{"model":"jev-latest","usage":{"tokens":3}}"#,
            Duration::ZERO,
        );
        let client = JevClient::with_key(config(url), Some("test-key".to_string())).unwrap();
        let result = client.ask(serde_json::json!({"q": "hi"})).await.unwrap();
        assert_eq!(result["model"], "jev-latest");

        let received = String::from_utf8_lossy(&task.join().unwrap()).to_ascii_lowercase();
        assert!(received.starts_with("post /v1/systemone"));
        assert!(received.contains("authorization: bearer test-key"));
    }

    #[tokio::test]
    async fn non_2xx_is_http_failure() {
        let (url, task) = server("422 Unprocessable Entity", "bad request", Duration::ZERO);
        let client = JevClient::with_key(config(url), Some("test-key".to_string())).unwrap();
        let err = client.ask(serde_json::json!({})).await.unwrap_err();
        assert_eq!(
            err,
            JevFailure::Http {
                status: 422,
                body: "bad request".to_string()
            }
        );
        task.join().unwrap();
    }

    #[tokio::test]
    async fn oversized_body_is_body_limit() {
        let body = "x".repeat(MAX_BODY + 1);
        let (url, task) = server("200 OK", &body, Duration::ZERO);
        let client = JevClient::with_key(config(url), Some("test-key".to_string())).unwrap();
        let err = client.ask(serde_json::json!({})).await.unwrap_err();
        assert_eq!(err, JevFailure::BodyLimit);
        task.join().unwrap();
    }

    #[tokio::test]
    async fn slow_server_is_timeout() {
        let (url, task) = server("200 OK", "{}", Duration::from_millis(200));
        let mut cfg = config(url);
        cfg.timeout = Duration::from_millis(30);
        let client = JevClient::with_key(cfg, Some("test-key".to_string())).unwrap();
        let err = client.ask(serde_json::json!({})).await.unwrap_err();
        assert_eq!(err, JevFailure::Timeout);
        task.join().unwrap();
    }

    #[tokio::test]
    async fn missing_key_is_unconfigured_without_a_connection() {
        // Bind a listener but never accept: if `ask` tried to connect it
        // would hang until the client-side timeout, not return instantly.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let client = JevClient::with_key(config(url), None).unwrap();
        let err = client.ask(serde_json::json!({})).await.unwrap_err();
        assert_eq!(err, JevFailure::Unconfigured);
    }

    #[tokio::test]
    async fn call_cap_is_enforced_after_max_calls() {
        // Base URL points at a bound-but-not-accepting listener: if the cap
        // check somehow ran after a connection attempt, the call would hang
        // or fail with Transport/Timeout instead of CallCap.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let mut cfg = config(url);
        cfg.max_calls = 0;
        let client = JevClient::with_key(cfg, Some("test-key".to_string())).unwrap();
        let err = client.ask(serde_json::json!({})).await.unwrap_err();
        assert_eq!(err, JevFailure::CallCap);
    }

    /// Live TypeSafe call. Opt-in: `TYPESAFE_API_KEY` must be set;
    /// `cargo test -p tidepool-handlers live_jev -- --ignored`.
    #[tokio::test]
    #[ignore = "calls the live TypeSafe API; needs TYPESAFE_API_KEY"]
    async fn live_jev_choice_round_trip() {
        let client = JevClient::new(JevConfig::default()).unwrap();
        assert!(client.configured(), "TYPESAFE_API_KEY is not set");
        let response = client
            .ask(serde_json::json!({
                "model": "jev-latest",
                "state": "A cat is sitting on a warm windowsill in the sun.",
                "questions": {
                    "place": {
                        "type": "choice",
                        "instructions": "Where is the cat?",
                        "criteria": {
                            "windowsill": "On a windowsill",
                            "roof": "On a roof",
                            "bed": "In a bed"
                        }
                    }
                }
            }))
            .await
            .unwrap();
        let answer = &response["answers"]["place"];
        assert_eq!(answer["type"], "choice", "{response}");
        assert_eq!(answer["choice"], "windowsill", "{response}");
        assert!(
            response["model"]
                .as_str()
                .is_some_and(|m| m.starts_with("jev")),
            "{response}"
        );
    }
}
