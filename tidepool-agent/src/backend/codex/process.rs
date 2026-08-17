//! `codex app-server` process lifecycle: spawn, complete the `initialize`
//! handshake, and drive one request/response round trip at a time, with
//! every JSONL frame captured for fixture recording.
//!
//! Built on [`codex_codes::RawAsyncClient`] rather than
//! [`codex_codes::AsyncClient`]: fixture recording needs the exact bytes on
//! the wire, and the raw client is the only layer that exposes them. The
//! typed request/response structs from `codex_codes` still do the
//! encoding/decoding — only the framing is manual.

use std::path::Path;
use std::time::{Duration, Instant};

use codex_codes::{
    AppServerBuilder, ClientInfo, InitializeCapabilities, InitializeParams, InitializeResponse,
    JsonRpcMessage, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse, RawAsyncClient,
    RequestId,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::backend::codex::transport::Transport;

/// How long a single request is allowed to wait for its matching response.
/// Metadata-only traffic here (no model tokens, no user-facing latency
/// budget to respect) — this bounds a hung or misbehaving process, not normal
/// round-trip time.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// How long shutdown is allowed to take before we give up waiting for the
/// child to be reaped and report it as a possible orphan.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

/// Which side of the wire produced a [`RecordedFrame`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameDirection {
    ClientToServer,
    ServerToClient,
}

/// One JSONL line observed on the wire, tagged with direction. Stored as a
/// parsed [`Value`] (not the raw string) so a fixture serializes
/// deterministically regardless of the sender's key order.
///
/// `Deserialize` as well as `Serialize`: recording and REPLAY read the same
/// type, so a recorded fixture and the replay transport that feeds it back
/// cannot drift apart in shape
/// (`crate::backend::codex::replay::TranscriptTransport`).
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    #[error("{method} response missing expected field: {field}")]
    MissingField { method: String, field: &'static str },
    #[error("codex app-server process may be orphaned: still alive {timeout:?} after shutdown")]
    OrphanedProcess { timeout: Duration },
    #[error("no item/tool/call is parked; {call_id} answers nothing")]
    NoParkedCall { call_id: String },
    #[error("reply answers call {answered} but {parked} is the parked call")]
    WrongCall { answered: String, parked: String },
    #[error(transparent)]
    Transport(#[from] codex_codes::Error),
}

/// A connected, initialized `codex app-server` process.
///
/// Every frame exchanged over its lifetime is available via
/// [`Session::frames`], in wire order, for fixture recording.
///
/// Generic over its [`Transport`], defaulting to the live one — see
/// [`crate::backend::codex::transport`] for why a default type parameter
/// rather than a boxed trait object. `Session` with no argument is the live
/// session, exactly as before.
pub struct Session<T = RawAsyncClient> {
    client: T,
    next_id: i64,
    frames: Vec<RecordedFrame>,
    pid: Option<u32>,
    /// State of the turn currently being pumped.
    turn: TurnState,
}

impl<T: Transport> Session<T> {
    /// A session over an ALREADY-connected transport, performing no handshake.
    ///
    /// [`Session::connect`] is the live path and does the handshake itself;
    /// this is the seam a replay transcript enters through, positioned wherever
    /// in a recorded conversation the caller wants the pump to pick up.
    pub fn over(client: T) -> Self {
        let pid = client.pid();
        Self {
            client,
            next_id: 1,
            frames: Vec::new(),
            pid,
            turn: TurnState::default(),
        }
    }

    /// The transport underneath, for a caller that needs to inspect it — a
    /// replay test asserting on what the pump actually wrote, in practice.
    pub fn transport(&self) -> &T {
        &self.client
    }
}

impl Session<RawAsyncClient> {
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
        let mut session = Self::over(raw);

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
}

impl<T: Transport> Session<T> {
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
    /// discarded — this path is for metadata requests with no concurrent
    /// thread/turn activity to interleave with.
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
    ///
    /// `pub(crate)` so the replay gates can drive the recorded handshake
    /// through the same code the live path uses, rather than around it.
    pub(crate) async fn notify(&mut self, method: &str) -> Result<(), SessionError> {
        let notif = JsonRpcNotification {
            method: method.to_string(),
            params: None,
        };
        self.send(&notif, method).await
    }

    async fn send<M: Serialize>(&mut self, message: &M, method: &str) -> Result<(), SessionError> {
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

/// One observed `item/tool/call`: the correlation triple as the server sent
/// it, and the park interval actually exercised (request received → reply
/// written) — measured, never manufactured; the driver replies as soon as
/// `on_tool_call` returns, with no artificial delay.
#[derive(Debug, Clone, Serialize)]
pub struct ObservedToolCall {
    pub thread_id: String,
    pub turn_id: String,
    pub call_id: String,
    pub tool: String,
    pub arguments: Value,
    pub park_duration_ms: u128,
}

/// Everything observed driving one turn to completion.
pub struct LiveTurnOutcome {
    pub turn_start_response: codex_codes::TurnStartResponse,
    pub tool_calls: Vec<ObservedToolCall>,
    pub turn: codex_codes::Turn,
}

/// Where the turn pump stopped.
///
/// [`TurnStop::ToolCall`] means the child's request is PARKED: this session
/// holds its JSON-RPC id and has written no response. Nothing on the wire moves
/// again until [`Session::reply_and_pump`] answers it, so the parent may take
/// as long as it needs — including running a whole Haskell handler with its own
/// effects. That is what makes the driving loop expressible in Haskell rather
/// than in a Rust callback.
#[derive(Debug)]
pub enum TurnStop {
    ToolCall(codex_codes::DynamicToolCallParams),
    Completed(codex_codes::Turn),
}

/// State the pump carries across one turn's stops, since a turn spans
/// several `pump` calls.
#[derive(Default)]
pub(crate) struct TurnState {
    /// The `turn/start` request id, until its response arrives.
    start_id: Option<RequestId>,
    turn_start_response: Option<codex_codes::TurnStartResponse>,
    /// The parked call's JSON-RPC id and the moment it arrived.
    parked: Option<(RequestId, String, Instant)>,
    tool_calls: Vec<ObservedToolCall>,
    /// The most recent `thread/tokenUsage/updated` totals. Cumulative per
    /// thread, so the LAST one seen is the turn's total — not a sum of deltas.
    usage: Option<codex_codes::ThreadTokenUsage>,
}

impl<T: Transport> Session<T> {
    /// The tool calls observed on the current turn, in order.
    pub fn observed_tool_calls(&self) -> &[ObservedToolCall] {
        &self.turn.tool_calls
    }

    /// The latest token-usage totals the server reported for this thread.
    pub fn token_usage(&self) -> Option<&codex_codes::ThreadTokenUsage> {
        self.turn.usage.as_ref()
    }

    /// Send `turn/start` and pump until the turn parks on a tool call or
    /// completes.
    ///
    /// A single continuous read loop rather than [`Session::request`] followed
    /// by a separate wait: `item/tool/call` can arrive BEFORE the `turn/start`
    /// response does, and [`Session::request`] would discard it while waiting
    /// for its own id, stranding the call.
    pub async fn start_turn(
        &mut self,
        turn_start: &codex_codes::TurnStartParams,
        timeout: Duration,
    ) -> Result<TurnStop, SessionError> {
        let method = codex_codes::methods::TURN_START;
        let start_id = RequestId::Integer(self.next_id);
        self.next_id += 1;
        self.turn = TurnState {
            start_id: Some(start_id.clone()),
            ..Default::default()
        };
        let req = JsonRpcRequest {
            id: start_id,
            method: method.to_string(),
            params: Some(serde_json::to_value(turn_start).map_err(|source| {
                SessionError::Encode {
                    method: method.to_string(),
                    source,
                }
            })?),
        };
        self.send(&req, method).await?;
        self.pump(timeout).await
    }

    /// Answer the parked `item/tool/call` and pump on to the next stop.
    ///
    /// `call_id` names the call being answered and is checked against the
    /// parked one — answering the wrong call is the misroute the correlation
    /// triple exists to make detectable, and it is refused here rather than
    /// written to the wire.
    pub async fn reply_and_pump(
        &mut self,
        call_id: &str,
        response: &codex_codes::DynamicToolCallResponse,
        timeout: Duration,
    ) -> Result<TurnStop, SessionError> {
        let Some((request_id, parked_call, received_at)) = self.turn.parked.take() else {
            return Err(SessionError::NoParkedCall {
                call_id: call_id.to_string(),
            });
        };
        if parked_call != call_id {
            // Put it back: refusing must not lose the call we are still
            // obliged to answer.
            self.turn.parked = Some((request_id, parked_call.clone(), received_at));
            return Err(SessionError::WrongCall {
                answered: call_id.to_string(),
                parked: parked_call,
            });
        }
        // Measured, never manufactured: however long the parent actually took.
        if let Some(observed) = self
            .turn
            .tool_calls
            .iter_mut()
            .find(|c| c.call_id == parked_call)
        {
            observed.park_duration_ms = received_at.elapsed().as_millis();
        }
        let resp = JsonRpcResponse {
            id: request_id,
            result: serde_json::to_value(response).map_err(|source| SessionError::Encode {
                method: "item/tool/call".to_string(),
                source,
            })?,
        };
        self.send(&resp, "item/tool/call reply").await?;
        self.pump(timeout).await
    }

    /// Read frames until the turn parks or completes.
    ///
    /// The `timeout` bounds THIS segment only, not the whole turn: a parent
    /// thinking for ten minutes between two pumps is not a hung backend, and
    /// charging its time against a backend liveness budget would kill
    /// healthy turns.
    async fn pump(&mut self, timeout: Duration) -> Result<TurnStop, SessionError> {
        tokio::time::timeout(timeout, self.pump_inner())
            .await
            .map_err(|_| SessionError::Timeout {
                method: codex_codes::methods::TURN_START.to_string(),
                timeout,
            })?
    }

    async fn pump_inner(&mut self) -> Result<TurnStop, SessionError> {
        let method = codex_codes::methods::TURN_START;
        loop {
            let Some(line) = self.client.next_line().await? else {
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
                JsonRpcMessage::Response(resp) if Some(&resp.id) == self.turn.start_id.as_ref() => {
                    self.turn.turn_start_response =
                        Some(serde_json::from_value(resp.result).map_err(|source| {
                            SessionError::Decode {
                                method: method.to_string(),
                                source,
                            }
                        })?);
                }
                JsonRpcMessage::Error(err) if Some(&err.id) == self.turn.start_id.as_ref() => {
                    return Err(SessionError::Rpc {
                        method: method.to_string(),
                        code: err.error.code,
                        message: err.error.message,
                    });
                }
                JsonRpcMessage::Request(req) if req.method == "item/tool/call" => {
                    let params: codex_codes::DynamicToolCallParams = req
                        .params
                        .ok_or_else(|| SessionError::MissingField {
                            method: "item/tool/call".to_string(),
                            field: "params",
                        })
                        .and_then(|p| {
                            serde_json::from_value(p).map_err(|source| SessionError::Decode {
                                method: "item/tool/call".to_string(),
                                source,
                            })
                        })?;
                    self.turn.tool_calls.push(ObservedToolCall {
                        thread_id: params.thread_id.clone(),
                        turn_id: params.turn_id.clone(),
                        call_id: params.call_id.clone(),
                        tool: params.tool.clone(),
                        arguments: params.arguments.clone(),
                        park_duration_ms: 0,
                    });
                    self.turn.parked = Some((req.id, params.call_id.clone(), Instant::now()));
                    return Ok(TurnStop::ToolCall(params));
                }
                // Approval requests SHOULD never arrive: turns run at
                // `approval_policy: Never` with containment enforced by the
                // sandbox policy (see `turn_start_params`). If one arrives
                // anyway (policy/protocol drift), DECLINE it in-protocol —
                // the child reads a clean refusal it can react to, instead
                // of the method-not-found error Codex surfaced as a
                // transport failure (the curator's first live run filed its
                // memories and then could not commit — dogfood 2026-08-14,
                // before `Never` was set). Never auto-accept here: a request
                // reaching this arm means policy said "ask", and this
                // headless seam has no one to ask.
                JsonRpcMessage::Request(req)
                    if req.method == codex_codes::methods::CMD_EXEC_APPROVAL
                        || req.method == codex_codes::methods::FILE_CHANGE_APPROVAL =>
                {
                    eprintln!(
                        "[tidepool-agent] approval request {} arrived under \
                         approval_policy=never — declining",
                        req.method
                    );
                    let result = if req.method == codex_codes::methods::CMD_EXEC_APPROVAL {
                        serde_json::to_value(
                            codex_codes::CommandExecutionRequestApprovalResponse::decline(),
                        )
                    } else {
                        serde_json::to_value(
                            codex_codes::FileChangeRequestApprovalResponse::decline(),
                        )
                    }
                    .map_err(|source| SessionError::Encode {
                        method: req.method.clone(),
                        source,
                    })?;
                    let resp = JsonRpcResponse { id: req.id, result };
                    self.send(&resp, "approval decline").await?;
                }
                JsonRpcMessage::Request(unexpected) => {
                    // Never leave a server request pending, even one this
                    // driver has no handler for.
                    let err_resp = codex_codes::JsonRpcError {
                        id: unexpected.id,
                        error: codex_codes::JsonRpcErrorData {
                            code: -32601,
                            message: format!(
                                "tidepool-agent does not handle {}",
                                unexpected.method
                            ),
                            data: None,
                        },
                    };
                    self.send(&err_resp, "unhandled server request").await?;
                }
                JsonRpcMessage::Notification(notif)
                    if notif.method == "thread/tokenUsage/updated" =>
                {
                    // Recorded rather than discarded: a caller that spends a
                    // real budget has to be able to say what it spent, and
                    // this is the only place the backend reports it.
                    if let Some(params) = notif.params {
                        let updated: codex_codes::ThreadTokenUsageUpdatedNotification =
                            serde_json::from_value(params).map_err(|source| {
                                SessionError::Decode {
                                    method: "thread/tokenUsage/updated".to_string(),
                                    source,
                                }
                            })?;
                        self.turn.usage = Some(updated.token_usage);
                    }
                }
                JsonRpcMessage::Notification(notif) if notif.method == "turn/completed" => {
                    let params = notif.params.ok_or_else(|| SessionError::MissingField {
                        method: "turn/completed".to_string(),
                        field: "params",
                    })?;
                    let completed: codex_codes::TurnCompletedNotification =
                        serde_json::from_value(params).map_err(|source| SessionError::Decode {
                            method: "turn/completed".to_string(),
                            source,
                        })?;
                    if self.turn.turn_start_response.is_none() {
                        return Err(SessionError::MissingField {
                            method: method.to_string(),
                            field: "response (turn/completed arrived first)",
                        });
                    }
                    return Ok(TurnStop::Completed(completed.turn));
                }
                // Everything else (item/started, item/updated, deltas, ...) is
                // already recorded above; not needed here.
                _ => continue,
            }
        }
    }

    /// Drive one turn to completion, answering every call via `on_tool_call`.
    ///
    /// A COMBINATOR over the pump, kept because the committed live `#[ignore]`d
    /// tests are existing evidence and should keep running unchanged. New code
    /// uses [`Session::start_turn`]/[`Session::reply_and_pump`] — a callback
    /// cannot run a Haskell handler.
    pub async fn drive_turn(
        &mut self,
        turn_start: &codex_codes::TurnStartParams,
        mut on_tool_call: impl FnMut(
            &codex_codes::DynamicToolCallParams,
        ) -> codex_codes::DynamicToolCallResponse,
        timeout: Duration,
    ) -> Result<LiveTurnOutcome, SessionError> {
        let mut stop = self.start_turn(turn_start, timeout).await?;
        loop {
            match stop {
                TurnStop::Completed(turn) => {
                    return Ok(LiveTurnOutcome {
                        turn_start_response: self
                            .turn
                            .turn_start_response
                            .clone()
                            .expect("pump refuses to complete without the turn/start response"),
                        tool_calls: std::mem::take(&mut self.turn.tool_calls),
                        turn,
                    })
                }
                TurnStop::ToolCall(params) => {
                    let response = on_tool_call(&params);
                    stop = self
                        .reply_and_pump(&params.call_id, &response, timeout)
                        .await?;
                }
            }
        }
    }
}

/// Whether a pid still names a live (or not-yet-reaped) process, via
/// `/proc` — Linux only, matching this environment.
fn process_exists(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

/// The text of the last `agentMessage` item in a completed turn — where
/// `outputSchema`-constrained structured output lands (the CLI source
/// documents `outputSchema` as constraining "the final assistant message for
/// this turn"; there is no separate structured-output field on [`Turn`] or
/// [`ThreadItem`](codex_codes::ThreadItem)). `None` if the turn has no agent
/// message at all.
pub fn last_agent_message_text(turn: &codex_codes::Turn) -> Option<&str> {
    turn.items.iter().rev().find_map(|item| match item {
        codex_codes::ThreadItem::AgentMessage { text, .. } => Some(text.as_str()),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::codex::isolation;
    use crate::backend::codex::isolation::ConfigSnapshot;

    fn turn_from_json(value: serde_json::Value) -> codex_codes::Turn {
        serde_json::from_value(value).expect("Turn fixture must match the real wire shape")
    }

    #[test]
    fn last_agent_message_text_finds_the_final_agent_message() {
        let turn = turn_from_json(serde_json::json!({
            "id": "turn_1",
            "status": "completed",
            "items": [
                {"type": "userMessage", "id": "i1", "content": []},
                {"type": "agentMessage", "id": "i2", "text": "{\"result\":\"first\"}"},
                {"type": "reasoning", "id": "i3"},
                {"type": "agentMessage", "id": "i4", "text": "{\"result\":\"final\"}"}
            ]
        }));
        assert_eq!(
            last_agent_message_text(&turn),
            Some("{\"result\":\"final\"}")
        );
    }

    #[test]
    fn last_agent_message_text_is_none_without_one() {
        let turn = turn_from_json(serde_json::json!({
            "id": "turn_1",
            "status": "completed",
            "items": [{"type": "reasoning", "id": "i1"}]
        }));
        assert_eq!(last_agent_message_text(&turn), None);
    }

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

    /// Evidence for `plans/post-restart/agent-lanes/dev-adapter-bringup.md`:
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

    fn ask_parent_tool_spec() -> crate::backend::codex::dynamic_tools::DynamicToolSpec {
        use crate::backend::codex::dynamic_tools::{DynamicToolFunctionSpec, DynamicToolSpec};
        DynamicToolSpec::Function(DynamicToolFunctionSpec {
            name: "ask_parent".to_string(),
            description:
                "Ask the parent a question when you need information you don't already have. \
                 Call this whenever you're missing a fact needed to complete the task."
                    .to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {"question": {"type": "string"}},
                "required": ["question"],
                "additionalProperties": false
            }),
            defer_loading: false,
        })
    }

    /// Free to retry: no `turn/start`, so no model tokens spent. Confirms
    /// the hand-rolled `dynamicTools` field and the `experimentalApi`
    /// opt-in are actually accepted by the real 0.146.0 server.
    #[tokio::test]
    #[ignore = "spawns a real app-server process against the operator's live ~/.codex; run explicitly"]
    async fn thread_start_with_dynamic_tools_is_accepted() {
        tokio::time::timeout(TEST_TIMEOUT, async {
            let mut session = Session::connect(InitializeCapabilities {
                experimental_api: Some(true),
                ..Default::default()
            })
            .await
            .expect("initialize handshake with experimentalApi=true");

            let params = crate::backend::codex::dynamic_tools::ThreadStartWithDynamicTools {
                ephemeral: true,
                dynamic_tools: vec![ask_parent_tool_spec()],
            };
            let response: codex_codes::ThreadStartResponse = session
                .request("thread/start", &params)
                .await
                .expect("thread/start with hand-rolled dynamicTools must be accepted");
            println!("thread/start accepted, threadId={}", response.thread.id);

            session
                .shutdown()
                .await
                .expect("clean shutdown, no orphaned process");
        })
        .await
        .expect("dry-run exceeded its bounded timeout");
    }

    /// The one live turn that spends the operator's ChatGPT tokens, pinned
    /// to `gpt-5.6-terra`. ONE attempt once `turn/start` is actually sent —
    /// do not loop this on failure; capture the frame log and report back
    /// instead.
    ///
    /// Ignored by default; run explicitly exactly once:
    ///
    ///     cargo test -p tidepool-agent --lib backend::codex::process::tests::phase4 -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "spends the operator's real ChatGPT tokens; run exactly once per go, never in a retry loop"]
    async fn phase4_live_vertical_ask_parent_round_trip() {
        tokio::time::timeout(Duration::from_secs(180), run_live_turn())
            .await
            .expect("phase 4 live turn exceeded its bounded timeout");
    }

    async fn run_live_turn() {
        let home = isolation::codex_home();
        let before = ConfigSnapshot::capture(&home).expect("capture pre-run config snapshot");
        let config_before_text =
            std::fs::read_to_string(home.join("config.toml")).unwrap_or_default();

        let workdir = tempfile::tempdir().expect("tempdir for the turn workspace");
        let workdir_path = workdir.path().to_string_lossy().to_string();

        const PASSPHRASE: &str = "tidepool-cobalt-7-do-not-reuse";

        let mut session = Session::connect(InitializeCapabilities {
            experimental_api: Some(true),
            ..Default::default()
        })
        .await
        .expect("initialize handshake with experimentalApi=true");

        let thread_params = crate::backend::codex::dynamic_tools::ThreadStartWithDynamicTools {
            ephemeral: true,
            dynamic_tools: vec![ask_parent_tool_spec()],
        };
        let thread_response: codex_codes::ThreadStartResponse = session
            .request("thread/start", &thread_params)
            .await
            .expect("thread/start with dynamicTools (cwd omitted)");
        let thread_id = thread_response.thread.id.clone();
        println!("thread/start ok, threadId={thread_id}");

        let turn_start = codex_codes::TurnStartParams {
            thread_id: thread_id.clone(),
            cwd: Some(workdir_path.clone()),
            model: Some("gpt-5.6-terra".to_string()),
            sandbox_policy: Some(codex_codes::SandboxPolicy::WorkspaceWrite {
                exclude_slash_tmp: Some(false),
                exclude_tmpdir_env_var: Some(false),
                network_access: Some(false),
                writable_roots: Some(vec![codex_codes::AbsolutePathBuf(workdir_path.clone())]),
            }),
            input: vec![codex_codes::UserInput::Text {
                text: format!(
                    "You have exactly one tool available, `ask_parent`, which takes a single \
                     string field `question`. To complete this task you must learn a secret \
                     passphrase that only the parent knows — you cannot guess it. Call \
                     ask_parent with a question asking for the passphrase. Once you receive it, \
                     respond with exactly {{\"result\": \"<passphrase>\"}} and nothing else. \
                     (expected length: {} characters)",
                    PASSPHRASE.len()
                ),
                text_elements: None,
            }],
            output_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {"result": {"type": "string"}},
                "required": ["result"],
                "additionalProperties": false
            })),
            ..Default::default()
        };

        let outcome = session
            .drive_turn(
                &turn_start,
                |call| {
                    println!(
                        "item/tool/call: threadId={} turnId={} callId={} tool={} arguments={}",
                        call.thread_id, call.turn_id, call.call_id, call.tool, call.arguments
                    );
                    if call.tool != "ask_parent" {
                        return codex_codes::DynamicToolCallResponse {
                            success: false,
                            content_items: vec![
                                codex_codes::DynamicToolCallOutputContentItem::InputText {
                                    text: format!("no such tool: {}", call.tool),
                                },
                            ],
                        };
                    }
                    codex_codes::DynamicToolCallResponse {
                        success: true,
                        content_items: vec![
                            codex_codes::DynamicToolCallOutputContentItem::InputText {
                                text: PASSPHRASE.to_string(),
                            },
                        ],
                    }
                },
                Duration::from_secs(150),
            )
            .await;

        // Persist frames and shut down BEFORE any assertion that could
        // panic — a failed turn still needs its evidence.
        let frames = session.frames().to_vec();
        let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/app-server-0.146.0/phase4-live-turn.jsonl");
        write_frames_jsonl(&frames, &fixture_path).expect("write phase 4 fixture");
        println!(
            "wrote {} frames to {}",
            frames.len(),
            fixture_path.display()
        );
        let _ = session.shutdown().await;

        let after = ConfigSnapshot::capture(&home).expect("capture post-run config snapshot");
        let report = before.compare(&after);
        println!("{}", report.render());
        let config_after_text =
            std::fs::read_to_string(home.join("config.toml")).unwrap_or_default();
        assert_eq!(
            config_before_text, config_after_text,
            "config.toml text changed across the live turn"
        );
        assert!(
            !config_after_text.contains(&workdir_path),
            "config.toml gained a reference to the turn's tempdir ({workdir_path}) — the \
             project-trust write cwd-at-turn-start is meant to avoid"
        );
        assert!(
            report.passed(),
            "config isolation violated across the live turn:\n{}",
            report.render()
        );

        let outcome = outcome.expect("live turn failed — see the committed frame log");

        println!("tool calls observed: {}", outcome.tool_calls.len());
        for call in &outcome.tool_calls {
            println!(
                "  correlation triple: threadId={} turnId={} callId={} tool={} park={}ms",
                call.thread_id, call.turn_id, call.call_id, call.tool, call.park_duration_ms
            );
        }
        assert_eq!(
            outcome.tool_calls.len(),
            1,
            "expected exactly one ask_parent call"
        );
        assert_eq!(outcome.tool_calls[0].thread_id, thread_id);
        assert_eq!(outcome.tool_calls[0].tool, "ask_parent");

        assert!(
            matches!(outcome.turn.status, codex_codes::TurnStatus::Completed),
            "turn did not complete: {:?}",
            outcome.turn.status
        );

        let text = last_agent_message_text(&outcome.turn)
            .expect("no agentMessage item in the completed turn");
        let decoded: serde_json::Value =
            serde_json::from_str(text).expect("final agent message was not valid JSON");
        assert_eq!(
            decoded["result"],
            serde_json::json!(PASSPHRASE),
            "structured result did not match the passphrase the parent supplied"
        );

        println!("PASS: resumed turn decoded to the exact passphrase the parent supplied");
    }
}
