//! `codex app-server` process lifecycle: spawn, complete the `initialize`
//! handshake, and drive one request/response round trip at a time, with
//! every JSONL frame captured for fixture recording.
//!
//! Built on [`codex_codes::RawAsyncClient`] rather than
//! [`codex_codes::AsyncClient`]: phase 3 needs the exact bytes on the wire
//! for the committed fixture, and the raw client is the only layer that
//! exposes them. The typed request/response structs from `codex_codes`
//! still do the encoding/decoding — only the framing is manual.

use std::path::Path;
use std::time::Duration;

use codex_codes::{
    AppServerBuilder, ClientInfo, InitializeCapabilities, InitializeParams, InitializeResponse,
    JsonRpcMessage, JsonRpcNotification, JsonRpcRequest, RawAsyncClient, RequestId,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

/// How long a single request is allowed to wait for its matching response.
/// Phase 3 traffic is metadata-only (no model tokens, no user-facing latency
/// budget to respect) — this bounds a hung or misbehaving process, not normal
/// round-trip time.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// How long shutdown is allowed to take before we give up waiting for the
/// child to be reaped and report it as a possible orphan.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

/// Which side of the wire produced a [`RecordedFrame`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameDirection {
    ClientToServer,
    ServerToClient,
}

/// One JSONL line observed on the wire, tagged with direction. Stored as a
/// parsed [`Value`] (not the raw string) so a fixture serializes
/// deterministically regardless of the sender's key order.
#[derive(Debug, Clone, Serialize)]
pub struct RecordedFrame {
    pub direction: FrameDirection,
    pub frame: Value,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("failed to spawn codex app-server: {0}")]
    Spawn(#[source] codex_codes::Error),
    #[error("request {method} timed out after {timeout:?}")]
    Timeout { method: String, timeout: Duration },
    #[error("app-server closed the connection before responding to {method}")]
    Closed { method: String },
    #[error("app-server returned a JSON-RPC error for {method}: ({code}) {message}")]
    Rpc {
        method: String,
        code: i64,
        message: String,
    },
    #[error("failed to decode response for {method}: {source}")]
    Decode {
        method: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to encode request for {method}: {source}")]
    Encode {
        method: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to parse a line from the app-server as JSON-RPC: {0}")]
    MalformedLine(#[source] serde_json::Error),
    #[error("codex app-server process may be orphaned: still alive {timeout:?} after shutdown")]
    OrphanedProcess { timeout: Duration },
    #[error(transparent)]
    Transport(#[from] codex_codes::Error),
}

/// A connected, initialized `codex app-server` process.
///
/// Every frame exchanged over its lifetime is available via
/// [`Session::frames`], in wire order, for fixture recording.
pub struct Session {
    client: RawAsyncClient,
    next_id: i64,
    frames: Vec<RecordedFrame>,
    pid: Option<u32>,
}

impl Session {
    /// Spawn `codex app-server` and complete the `initialize` handshake.
    ///
    /// Inherits the parent process's environment (in particular `HOME`, and
    /// therefore `~/.codex`) rather than pointing at an isolated
    /// `CODEX_HOME` — proving normal runs don't mutate the operator's real
    /// config is the point of this adapter, not something to route around.
    pub async fn connect(capabilities: InitializeCapabilities) -> Result<Self, SessionError> {
        let raw = RawAsyncClient::start_with(AppServerBuilder::new())
            .await
            .map_err(SessionError::Spawn)?;
        let pid = raw.pid();
        let mut session = Self {
            client: raw,
            next_id: 1,
            frames: Vec::new(),
            pid,
        };

        let init_params = InitializeParams {
            client_info: ClientInfo {
                name: "tidepool-agent".to_string(),
                title: None,
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            capabilities: Some(capabilities),
        };
        let _: InitializeResponse = session
            .request(codex_codes::methods::INITIALIZE, &init_params)
            .await?;
        session.notify(codex_codes::methods::INITIALIZED).await?;
        Ok(session)
    }

    /// Every frame exchanged so far, in wire order.
    pub fn frames(&self) -> &[RecordedFrame] {
        &self.frames
    }

    /// The app-server child process's OS pid, when the platform reports one.
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Send a JSON-RPC request and wait for its matching response.
    ///
    /// Any other traffic that arrives first (notifications, unrelated server
    /// requests) is still recorded into [`Session::frames`] but otherwise
    /// discarded — phase 3 only exercises metadata requests with no
    /// concurrent thread/turn activity to interleave with.
    pub async fn request<P: Serialize, R: DeserializeOwned>(
        &mut self,
        method: &str,
        params: &P,
    ) -> Result<R, SessionError> {
        let id = RequestId::Integer(self.next_id);
        self.next_id += 1;

        let req = JsonRpcRequest {
            id: id.clone(),
            method: method.to_string(),
            params: Some(
                serde_json::to_value(params).map_err(|source| SessionError::Encode {
                    method: method.to_string(),
                    source,
                })?,
            ),
        };
        self.send(&req, method).await?;

        let deadline = tokio::time::Instant::now() + REQUEST_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let line = tokio::time::timeout(remaining, self.client.next_line())
                .await
                .map_err(|_| SessionError::Timeout {
                    method: method.to_string(),
                    timeout: REQUEST_TIMEOUT,
                })??;
            let Some(line) = line else {
                return Err(SessionError::Closed {
                    method: method.to_string(),
                });
            };
            let value: Value = serde_json::from_str(&line).map_err(SessionError::MalformedLine)?;
            self.frames.push(RecordedFrame {
                direction: FrameDirection::ServerToClient,
                frame: value.clone(),
            });
            let msg: JsonRpcMessage =
                serde_json::from_value(value).map_err(SessionError::MalformedLine)?;
            match msg {
                JsonRpcMessage::Response(resp) if resp.id == id => {
                    return serde_json::from_value(resp.result).map_err(|source| {
                        SessionError::Decode {
                            method: method.to_string(),
                            source,
                        }
                    });
                }
                JsonRpcMessage::Error(err) if err.id == id => {
                    return Err(SessionError::Rpc {
                        method: method.to_string(),
                        code: err.error.code,
                        message: err.error.message,
                    });
                }
                // Not our response — a notification, a server request, or a
                // response to some other id. Already recorded above; keep
                // waiting for ours.
                _ => continue,
            }
        }
    }

    /// Send a JSON-RPC notification (no response expected).
    async fn notify(&mut self, method: &str) -> Result<(), SessionError> {
        let notif = JsonRpcNotification {
            method: method.to_string(),
            params: None,
        };
        self.send(&notif, method).await
    }

    async fn send<T: Serialize>(&mut self, message: &T, method: &str) -> Result<(), SessionError> {
        let value = serde_json::to_value(message).map_err(|source| SessionError::Encode {
            method: method.to_string(),
            source,
        })?;
        self.frames.push(RecordedFrame {
            direction: FrameDirection::ClientToServer,
            frame: value.clone(),
        });
        self.client.send(&value).await?;
        Ok(())
    }

    /// Kill the app-server process and confirm it no longer exists — never a
    /// bare "the kill call returned Ok", since that only proves the signal
    /// was sent.
    pub async fn shutdown(self) -> Result<(), SessionError> {
        let pid = self.pid;
        self.client.shutdown().await?;
        let Some(pid) = pid else {
            return Ok(());
        };
        let deadline = tokio::time::Instant::now() + SHUTDOWN_TIMEOUT;
        while process_exists(pid) {
            if tokio::time::Instant::now() >= deadline {
                return Err(SessionError::OrphanedProcess {
                    timeout: SHUTDOWN_TIMEOUT,
                });
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Ok(())
    }
}

/// Whether a pid still names a live (or not-yet-reaped) process, via
/// `/proc` — Linux only, matching this environment.
fn process_exists(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::codex::isolation;
    use crate::backend::codex::isolation::ConfigSnapshot;

    /// Write recorded frames as newline-delimited JSON, one frame per line,
    /// in wire order. Test-only: the only consumer is the fixture-recording
    /// run below.
    fn write_frames_jsonl(frames: &[RecordedFrame], path: &Path) -> std::io::Result<()> {
        use std::io::Write;
        let mut out = String::new();
        for frame in frames {
            out.push_str(&serde_json::to_string(frame).expect("RecordedFrame always serializes"));
            out.push('\n');
        }
        std::fs::File::create(path)?.write_all(out.as_bytes())
    }

    const TEST_TIMEOUT: Duration = Duration::from_secs(60);

    /// Phase 3 (`plans/post-restart/agent-lanes/dev-adapter-bringup.md`):
    /// spawn the real app-server against the operator's real `~/.codex`,
    /// complete the handshake, make one metadata request that spends no
    /// model tokens, shut down cleanly, and prove config isolation held
    /// across the whole run.
    ///
    /// Ignored by default — this is a live process against the operator's
    /// installation, not something the fast default tier should run
    /// unattended. Run explicitly:
    ///
    ///     cargo test -p tidepool-agent --lib backend::codex::process::tests::handshake -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "spawns a real app-server process against the operator's live ~/.codex; run explicitly"]
    async fn handshake_and_model_list_leave_config_untouched() {
        tokio::time::timeout(TEST_TIMEOUT, run())
            .await
            .expect("phase 3 handshake test exceeded its bounded timeout");
    }

    async fn run() {
        let home = isolation::codex_home();
        let before = ConfigSnapshot::capture(&home).expect("capture pre-run config snapshot");

        let mut session = Session::connect(InitializeCapabilities::default())
            .await
            .expect("initialize handshake");

        let response: codex_codes::ModelListResponse = session
            .request(
                codex_codes::methods::MODEL_LIST,
                &codex_codes::ModelListParams::default(),
            )
            .await
            .expect("model/list");
        assert!(
            !response.data.is_empty(),
            "model/list returned no models — unexpected for an authenticated session"
        );

        let frames = session.frames().to_vec();
        session
            .shutdown()
            .await
            .expect("clean shutdown, no orphaned process");

        let after = ConfigSnapshot::capture(&home).expect("capture post-run config snapshot");
        let report = before.compare(&after);
        println!("{}", report.render());
        assert!(
            report.passed(),
            "config isolation violated:\n{}",
            report.render()
        );

        let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/app-server-0.146.0/phase3-handshake.jsonl");
        write_frames_jsonl(&frames, &fixture_path).expect("write phase 3 fixture");
        println!(
            "wrote {} frames to {}",
            frames.len(),
            fixture_path.display()
        );
    }
}
