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
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use reqwest::header::{HeaderValue, AUTHORIZATION};

/// Response bytes are streamed and capped here — same limit as the research
/// transport.
const MAX_BODY: usize = 2 * 1024 * 1024;

/// Bytes of a non-2xx body kept for `JevFailure::Http`.
const MAX_ERROR_BODY: usize = 2000;
const DEFAULT_CIRCUIT_COOLDOWN: Duration = Duration::from_secs(30);
const TRANSIENT_PROBE_DELAY: Duration = Duration::from_secs(5);

pub struct JevConfig {
    pub base_url: String,
    pub timeout: Duration,
    pub max_calls: u64,
    /// Time between probes after an account-level Jev failure.
    pub circuit_cooldown: Duration,
}

impl Default for JevConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.typesafe.ai".to_string(),
            timeout: Duration::from_secs(15),
            max_calls: 100_000,
            circuit_cooldown: DEFAULT_CIRCUIT_COOLDOWN,
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
    #[error("Jev client setup failed: {0}")]
    ClientSetup(String),
    #[error("Jev circuit open after HTTP {status}; retry after {retry_after_ms} ms")]
    CircuitOpen { status: u16, retry_after_ms: u64 },
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
    circuit: Mutex<CircuitState>,
}

// One client is shared by every actor in a forest. The gate only pauses
// requests for that client's credential; it never couples separate runs or
// accounts. The epoch prevents an older in-flight request from reopening a
// circuit after a successful recovery probe.
enum CircuitState {
    Closed {
        epoch: u64,
    },
    Open {
        epoch: u64,
        status: u16,
        until: Instant,
        suppressed: u64,
    },
    Probing {
        epoch: u64,
        status: u16,
        suppressed: u64,
    },
}

#[derive(Clone, Copy)]
enum Permit {
    Regular(u64),
    Probe(u64),
}

// A cancelled ask must release its probe slot. The request future can be
// dropped while awaiting HTTP, before `finish` has a result to inspect.
struct ProbeGuard<'a> {
    circuit: &'a Mutex<CircuitState>,
    permit: Permit,
    finished: bool,
}

impl Drop for ProbeGuard<'_> {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        if let Permit::Probe(epoch) = self.permit {
            self.circuit.lock().cancel_probe(epoch);
        }
    }
}

impl CircuitState {
    fn admit(&mut self) -> Result<Permit, JevFailure> {
        match self {
            Self::Closed { epoch } => Ok(Permit::Regular(*epoch)),
            Self::Open {
                epoch,
                status,
                until,
                suppressed,
            } if Instant::now() >= *until => {
                let permit = Permit::Probe(*epoch);
                *self = Self::Probing {
                    epoch: *epoch,
                    status: *status,
                    suppressed: *suppressed,
                };
                Ok(permit)
            }
            Self::Open {
                status,
                until,
                suppressed,
                ..
            } => {
                *suppressed += 1;
                Err(JevFailure::CircuitOpen {
                    status: *status,
                    retry_after_ms: until.saturating_duration_since(Instant::now()).as_millis()
                        as u64,
                })
            }
            Self::Probing {
                status, suppressed, ..
            } => {
                *suppressed += 1;
                Err(JevFailure::CircuitOpen {
                    status: *status,
                    retry_after_ms: 0,
                })
            }
        }
    }

    fn finish(
        &mut self,
        permit: Permit,
        result: &Result<serde_json::Value, JevFailure>,
        cooldown: Duration,
    ) {
        let (epoch, was_probe) = match permit {
            Permit::Regular(epoch) => (epoch, false),
            Permit::Probe(epoch) => (epoch, true),
        };
        let current_epoch = match self {
            Self::Closed { epoch } | Self::Open { epoch, .. } | Self::Probing { epoch, .. } => {
                *epoch
            }
        };
        if epoch != current_epoch {
            return;
        }
        let account_status = result.as_ref().err().and_then(account_failure_status);
        match (was_probe, account_status) {
            (false, Some(status)) if matches!(self, Self::Closed { .. }) => {
                tracing::warn!(status, "jev circuit opened after account failure");
                *self = Self::Open {
                    epoch: epoch + 1,
                    status,
                    until: Instant::now() + cooldown,
                    suppressed: 0,
                };
            }
            (true, Some(status)) if matches!(self, Self::Probing { .. }) => {
                let suppressed = self.suppressed();
                tracing::warn!(
                    status,
                    suppressed,
                    "jev circuit probe found account failure"
                );
                *self = Self::Open {
                    epoch: epoch + 1,
                    status,
                    until: Instant::now() + cooldown,
                    suppressed: 0,
                };
            }
            (true, None) if matches!(self, Self::Probing { .. }) => {
                if result.is_ok() || result.as_ref().err().is_some_and(is_caller_error) {
                    tracing::info!(suppressed = self.suppressed(), "jev circuit recovered");
                    *self = Self::Closed { epoch: epoch + 1 };
                } else {
                    let status = self.status();
                    tracing::warn!(status, "jev circuit probe failed transiently");
                    *self = Self::Open {
                        epoch: epoch + 1,
                        status,
                        until: Instant::now() + TRANSIENT_PROBE_DELAY,
                        suppressed: 0,
                    };
                }
            }
            _ => {}
        }
    }

    fn cancel_probe(&mut self, epoch: u64) {
        if let Self::Probing {
            epoch: current,
            status,
            suppressed,
        } = self
        {
            if *current == epoch {
                tracing::warn!(status, suppressed, "jev circuit probe cancelled");
                *self = Self::Open {
                    epoch: epoch + 1,
                    status: *status,
                    until: Instant::now() + TRANSIENT_PROBE_DELAY,
                    suppressed: 0,
                };
            }
        }
    }

    fn status(&self) -> u16 {
        match self {
            Self::Open { status, .. } | Self::Probing { status, .. } => *status,
            Self::Closed { .. } => unreachable!("closed circuit has no status"),
        }
    }

    fn suppressed(&self) -> u64 {
        match self {
            Self::Open { suppressed, .. } | Self::Probing { suppressed, .. } => *suppressed,
            Self::Closed { .. } => 0,
        }
    }
}

fn account_failure_status(failure: &JevFailure) -> Option<u16> {
    match failure {
        JevFailure::Http { status, .. } if matches!(*status, 401 | 403) => Some(*status),
        JevFailure::Http { status: 402, body } => {
            let parsed: serde_json::Value = serde_json::from_str(body).ok()?;
            let detail = parsed.get("detail").unwrap_or(&parsed);
            (detail.get("error_type").and_then(serde_json::Value::as_str) == Some("billing_error"))
                .then_some(402)
        }
        _ => None,
    }
}

fn is_caller_error(failure: &JevFailure) -> bool {
    matches!(
        failure,
        JevFailure::Http {
            status: 400 | 422,
            ..
        }
    )
}

impl JevClient {
    /// Resolves the key from env/secrets and retains client construction errors.
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
            .map_err(|e| JevFailure::ClientSetup(e.to_string()))?;
        Ok(Self {
            http,
            key,
            config,
            calls: AtomicU64::new(0),
            circuit: Mutex::new(CircuitState::Closed { epoch: 0 }),
        })
    }

    pub fn configured(&self) -> bool {
        self.key.is_some()
    }

    pub async fn ask(&self, body: serde_json::Value) -> Result<serde_json::Value, JevFailure> {
        let Some(key) = self.key.as_deref() else {
            return Err(JevFailure::Unconfigured);
        };
        if self.calls.load(Ordering::Relaxed) >= self.config.max_calls {
            return Err(JevFailure::CallCap);
        }
        let permit = self.circuit.lock().admit()?;
        let mut probe_guard = ProbeGuard {
            circuit: &self.circuit,
            permit,
            finished: false,
        };
        if self
            .calls
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                (count < self.config.max_calls).then(|| count + 1)
            })
            .is_err()
        {
            // A concurrent caller claimed the last budget slot. No provider
            // response exists to establish whether an open circuit recovered.
            return Err(JevFailure::CallCap);
        }

        let result = self.send(key, body).await;
        self.circuit
            .lock()
            .finish(permit, &result, self.config.circuit_cooldown);
        probe_guard.finished = true;
        result
    }

    async fn send(
        &self,
        key: &str,
        body: serde_json::Value,
    ) -> Result<serde_json::Value, JevFailure> {
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
    use std::sync::Arc;
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
            circuit_cooldown: Duration::from_millis(200),
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

    fn scripted_server(
        replies: Vec<(&'static str, &'static str)>,
    ) -> (String, thread::JoinHandle<usize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = thread::spawn(move || {
            for (status, body) in &replies {
                let (mut stream, _) = listener.accept().unwrap();
                let mut received = Vec::new();
                let mut buffer = [0; 4096];
                while !received.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    let count = stream.read(&mut buffer).unwrap();
                    if count == 0 {
                        break;
                    }
                    received.extend_from_slice(&buffer[..count]);
                }
                let reply = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(reply.as_bytes()).unwrap();
            }
            replies.len()
        });
        (url, task)
    }

    const CREDIT_ERROR: &str =
        r#"{"detail":{"error_type":"billing_error","message":"no available credits"}}"#;

    #[tokio::test]
    async fn billing_failure_opens_then_one_probe_recovers_after_replenishment() {
        let (url, task) = scripted_server(vec![
            ("402 Payment Required", CREDIT_ERROR),
            ("200 OK", r#"{"model":"jev-latest","usage":{}}"#),
            ("200 OK", r#"{"model":"jev-latest","usage":{}}"#),
        ]);
        let client = JevClient::with_key(config(url), Some("test-key".into())).unwrap();
        assert!(matches!(
            client.ask(serde_json::json!({})).await,
            Err(JevFailure::Http { status: 402, .. })
        ));
        assert!(matches!(
            client.ask(serde_json::json!({})).await,
            Err(JevFailure::CircuitOpen { status: 402, .. })
        ));
        assert_eq!(client.calls.load(Ordering::Relaxed), 1);
        tokio::time::sleep(Duration::from_millis(220)).await;
        assert_eq!(
            client.ask(serde_json::json!({})).await.unwrap()["model"],
            "jev-latest"
        );
        assert_eq!(
            client.ask(serde_json::json!({})).await.unwrap()["model"],
            "jev-latest"
        );
        assert_eq!(client.calls.load(Ordering::Relaxed), 3);
        assert_eq!(task.join().unwrap(), 3);
    }

    #[test]
    fn concurrent_old_failures_cannot_reopen_after_recovery_and_only_one_probe_enters() {
        let mut gate = CircuitState::Closed { epoch: 0 };
        let first = gate.admit().unwrap();
        let second = gate.admit().unwrap();
        let failure = Err(JevFailure::Http {
            status: 402,
            body: CREDIT_ERROR.into(),
        });
        gate.finish(first, &failure, Duration::ZERO);
        let probe = gate.admit().unwrap();
        assert!(matches!(probe, Permit::Probe(_)));
        assert!(matches!(
            gate.admit(),
            Err(JevFailure::CircuitOpen { status: 402, .. })
        ));
        gate.finish(
            probe,
            &Ok(serde_json::json!({"model":"jev-latest"})),
            Duration::ZERO,
        );
        gate.finish(second, &failure, Duration::ZERO);
        assert!(matches!(gate.admit(), Ok(Permit::Regular(_))));
    }

    #[tokio::test]
    async fn cancelled_probe_releases_slot_for_later_recovery() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut cfg = config(format!("http://{}", listener.local_addr().unwrap()));
        cfg.timeout = Duration::from_secs(2);
        let client = Arc::new(JevClient::with_key(cfg, Some("test-key".into())).unwrap());
        *client.circuit.lock() = CircuitState::Open {
            epoch: 1,
            status: 402,
            until: Instant::now(),
            suppressed: 0,
        };
        let probe_client = Arc::clone(&client);
        let probe = tokio::spawn(async move { probe_client.ask(serde_json::json!({})).await });
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if matches!(*client.circuit.lock(), CircuitState::Probing { .. }) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        probe.abort();
        assert!(probe.await.unwrap_err().is_cancelled());
        let mut gate = client.circuit.lock();
        assert!(matches!(*gate, CircuitState::Open { .. }));
        if let CircuitState::Open { until, .. } = &mut *gate {
            *until = Instant::now();
        }
        assert!(matches!(gate.admit(), Ok(Permit::Probe(_))));
    }

    #[tokio::test]
    async fn exhausted_call_budget_does_not_establish_probe_recovery() {
        let (url, task) = scripted_server(vec![("402 Payment Required", CREDIT_ERROR)]);
        let mut cfg = config(url);
        cfg.max_calls = 1;
        let client = JevClient::with_key(cfg, Some("test-key".into())).unwrap();
        assert!(matches!(
            client.ask(serde_json::json!({})).await,
            Err(JevFailure::Http { status: 402, .. })
        ));
        tokio::time::sleep(Duration::from_millis(220)).await;
        assert_eq!(
            client.ask(serde_json::json!({})).await,
            Err(JevFailure::CallCap)
        );
        assert!(matches!(*client.circuit.lock(), CircuitState::Open { .. }));
        assert_eq!(client.calls.load(Ordering::Relaxed), 1);
        assert_eq!(task.join().unwrap(), 1);
    }

    #[tokio::test]
    async fn transient_and_caller_local_http_failures_do_not_open_circuit() {
        let (url, task) = scripted_server(vec![
            ("429 Too Many Requests", "rate limited"),
            ("422 Unprocessable Entity", "bad question"),
            ("200 OK", r#"{"model":"jev-latest","usage":{}}"#),
        ]);
        let client = JevClient::with_key(config(url), Some("test-key".into())).unwrap();
        assert!(matches!(
            client.ask(serde_json::json!({})).await,
            Err(JevFailure::Http { status: 429, .. })
        ));
        assert!(matches!(
            client.ask(serde_json::json!({})).await,
            Err(JevFailure::Http { status: 422, .. })
        ));
        assert_eq!(
            client.ask(serde_json::json!({})).await.unwrap()["model"],
            "jev-latest"
        );
        assert_eq!(task.join().unwrap(), 3);
    }

    #[test]
    fn transient_probe_failure_keeps_circuit_open_and_caller_error_recovers_it() {
        let mut gate = CircuitState::Closed { epoch: 0 };
        let first = gate.admit().unwrap();
        gate.finish(
            first,
            &Err(JevFailure::Http {
                status: 401,
                body: "invalid key".into(),
            }),
            Duration::ZERO,
        );
        let probe = gate.admit().unwrap();
        gate.finish(probe, &Err(JevFailure::Timeout), Duration::ZERO);
        assert!(matches!(
            gate.admit(),
            Err(JevFailure::CircuitOpen { status: 401, .. })
        ));
        if let CircuitState::Open { until, .. } = &mut gate {
            *until = Instant::now();
        }
        let probe = gate.admit().unwrap();
        gate.finish(
            probe,
            &Err(JevFailure::Http {
                status: 422,
                body: "bad question".into(),
            }),
            Duration::ZERO,
        );
        assert!(matches!(gate.admit(), Ok(Permit::Regular(_))));
    }

    #[test]
    fn unrelated_payment_error_does_not_open_account_circuit() {
        let mut gate = CircuitState::Closed { epoch: 0 };
        let first = gate.admit().unwrap();
        gate.finish(
            first,
            &Err(JevFailure::Http {
                status: 402,
                body: "payment required".into(),
            }),
            Duration::ZERO,
        );
        assert!(matches!(gate.admit(), Ok(Permit::Regular(_))));
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
