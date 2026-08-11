//! Drive the PRODUCTION [`Session`](super::process::Session) pump over a
//! RECORDED transcript, with no process and no model call.
//!
//! # Why a recording rather than a simulator
//!
//! Protocol behavior — frame ordering, the parked correlation triple, the
//! `success:false` reply shape, `turn/completed` projection,
//! `thread/tokenUsage/updated` capture — has to be proven by the real adapter
//! against real bytes, not encoded by hand in
//! [`crate::backend::mock::MockBackend`]: a hand-written imitation can only
//! ever encode what its author believed the server does. Mock policy:
//! `plans/post-restart/agent-lanes/codex-live-frozen-contract.md`.
//! `tidepool-harness`'s `ReplayProvider` is the named precedent for this
//! shape.
//!
//! The frames replayed here were produced by one recorded live turn
//! (`fixtures/app-server-0.146.0/phase4-live-turn.jsonl`, 35 frames). That run
//! happened to be on `gpt-5.6-terra`; that is a fact about the bytes and
//! nothing else. Nothing in this module reads, resolves, or selects a model —
//! model policy lives in `driver.rs`'s allowlist, which this file never
//! touches.
//!
//! # Two things the transcript cannot give you for free
//!
//! **Request ids are minted by the live pump, not read from the recording.**
//! [`Session`](super::process::Session) mints `1, 2, 3, …` from its own
//! counter, and a test that starts mid-conversation mints different integers
//! than the recorded run did (the recording's `turn/start` is id 3 because
//! `initialize` and `thread/start` came first; a test driving only the turn
//! mints id 1). So the transport LEARNS the mapping instead of assuming it:
//! each client→server frame the pump writes is paired with the next recorded
//! client→server frame by METHOD, and the recorded id is remembered as an alias
//! for the id the pump actually used. Recorded server→client RESPONSES are then
//! rewritten through that map on the way out.
//!
//! Matching by method rather than by ordinal is the deliberate choice: ordinal
//! matching silently mispairs the moment a test drives only part of the
//! recorded session, which is the normal case here, and a mispair would show up
//! as an unrelated decode failure ten frames later. Method matching either
//! pairs correctly or fails immediately, naming both frames.
//!
//! Ids the SERVER minted (the `item/tool/call` request id) are never rewritten
//! — the pump echoes them back verbatim, so they are already correct, and
//! rewriting them would be the one way to corrupt a correlation the tests exist
//! to check. Concretely: only frames that are responses (an `id`, a `result` or
//! `error`, and no `method`) are eligible for rewriting.
//!
//! **A recording ends.** When the frames run out, [`Transport::next_line`]
//! returns `None`, which the pump reports as
//! [`SessionError::Closed`](super::process::SessionError::Closed) — a clean,
//! named failure rather than a test that hangs to its timeout.
//!
//! # What this replays, and what it deliberately does not
//!
//! It replays [`Session`](super::process::Session) — the pump. It does NOT
//! replay [`CodexAgentBackend`](super::CodexAgentBackend), for two reasons that
//! are facts about the recording rather than preferences:
//!
//! 1. `CodexAgentBackend::start_turn` resolves a model first, which issues a
//!    `model/list` request. **The recording contains no `model/list`
//!    exchange** — it was recorded from a session that had already resolved.
//!    Replaying the backend would therefore mean writing a `model/list`
//!    response by hand, which is exactly the invented frame the record/replay
//!    policy exists to avoid.
//! 2. The backend owns its own tokio runtime and builds its `Session` through
//!    `Session::connect`, which spawns a process. Reaching it would take a
//!    second constructor and a transport type parameter threaded through
//!    `driver.rs`.
//!
//! A future recording taken through the backend (one that includes
//! `model/list`) makes this cheap; until then the pump is where the protocol
//! behavior lives and where it is proven.

use std::collections::HashMap;
use std::future::{ready, Future};
use std::path::Path;

use serde_json::Value;

use crate::backend::codex::process::{FrameDirection, RecordedFrame};
use crate::backend::codex::transport::Transport;

/// A transcript that could not be loaded, or asked for a position it does not
/// contain. Distinct from a REPLAY mismatch, which happens mid-pump and
/// surfaces through the transport as `codex_codes::Error::Protocol`.
#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error("failed to read the transcript {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("transcript {path} line {line} is not a RecordedFrame: {source}")]
    Parse {
        path: String,
        line: usize,
        #[source]
        source: serde_json::Error,
    },
    #[error("transcript {path} has no client->server request for {method}")]
    NoSuchRequest { path: String, method: String },
}

/// A [`Transport`] that serves a recorded conversation instead of a process.
///
/// One cursor walks the transcript in wire order, so ORDER is enforced rather
/// than assumed: a `next_line` that lands on a client→server frame the pump has
/// not written yet is a mismatch, not a skip. That is what makes "the pump has
/// written no response yet, and the recording agrees" a checkable claim.
pub struct TranscriptTransport {
    /// Names the transcript in every mismatch message.
    label: String,
    frames: Vec<RecordedFrame>,
    cursor: usize,
    /// Every frame the pump actually wrote, in order — the evidence a test
    /// asserts on when the claim is about what the pump did or did not send.
    sent: Vec<Value>,
    /// Recorded client-minted request id → the id the live pump used instead.
    aliases: HashMap<String, Value>,
}

impl TranscriptTransport {
    /// Load a recorded `.jsonl` transcript: one
    /// [`RecordedFrame`] per line, in wire order.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, ReplayError> {
        let path = path.as_ref();
        let label = path.display().to_string();
        let text = std::fs::read_to_string(path).map_err(|source| ReplayError::Read {
            path: label.clone(),
            source,
        })?;
        let mut frames = Vec::new();
        for (index, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            frames.push(
                serde_json::from_str(line).map_err(|source| ReplayError::Parse {
                    path: label.clone(),
                    line: index + 1,
                    source,
                })?,
            );
        }
        Ok(Self {
            label,
            frames,
            cursor: 0,
            sent: Vec::new(),
            aliases: HashMap::new(),
        })
    }

    /// Position the cursor at the recorded client→server request for `method`,
    /// discarding everything before it.
    ///
    /// A test that drives only [`Session::start_turn`](super::process::Session::start_turn)
    /// never performs the handshake or `thread/start`, so those recorded frames
    /// have no live counterpart to pair with. Dropping them is honest — the
    /// alternative, letting `next_line` skip client frames it has not seen a
    /// send for, would give up order enforcement for every frame, not just the
    /// prefix.
    pub fn resuming_at(mut self, method: &str) -> Result<Self, ReplayError> {
        let position = self.frames.iter().position(|frame| {
            frame.direction == FrameDirection::ClientToServer
                && method_of(&frame.frame) == Some(method)
        });
        let Some(position) = position else {
            return Err(ReplayError::NoSuchRequest {
                path: self.label,
                method: method.to_string(),
            });
        };
        self.frames.drain(..position);
        Ok(self)
    }

    /// Every frame the pump wrote, in order.
    pub fn sent(&self) -> &[Value] {
        &self.sent
    }

    /// How many recorded frames have not been consumed yet.
    pub fn remaining(&self) -> usize {
        self.frames.len().saturating_sub(self.cursor)
    }

    fn serve(&mut self) -> Result<Option<String>, codex_codes::Error> {
        let Some(recorded) = self.frames.get(self.cursor) else {
            return Ok(None);
        };
        if recorded.direction == FrameDirection::ClientToServer {
            return Err(mismatch(format!(
                "{}: the recording's next frame is the client's {}, but the pump asked to read \
                 instead of write",
                self.label,
                describe(&recorded.frame)
            )));
        }
        let mut frame = recorded.frame.clone();
        self.cursor += 1;
        self.apply_alias(&mut frame);
        Ok(Some(frame.to_string()))
    }

    fn accept(&mut self, written: &Value) -> Result<(), codex_codes::Error> {
        let Some(recorded) = self.frames.get(self.cursor) else {
            return Err(mismatch(format!(
                "{}: the recording is exhausted, but the pump wrote {}",
                self.label,
                describe(written)
            )));
        };
        if recorded.direction == FrameDirection::ServerToClient {
            return Err(mismatch(format!(
                "{}: the recording's next frame is the server's {}, but the pump wrote {}",
                self.label,
                describe(&recorded.frame),
                describe(written)
            )));
        }
        match (method_of(&recorded.frame), method_of(written)) {
            // A request or notification: the METHOD is the identity. Params are
            // deliberately not compared — a replay test supplies its own tool
            // answer, and demanding byte-equality would make the fixture a
            // straitjacket instead of a record of protocol shape.
            (Some(expected), Some(actual)) if expected == actual => {}
            // A response to a server-minted request: the id IS the correlation,
            // and it came from the recording, so it must match exactly. This is
            // the check that catches a misrouted reply.
            (None, None) if recorded.frame.get("id") == written.get("id") => {}
            _ => {
                return Err(mismatch(format!(
                    "{}: the recording expected the client to write {}, but the pump wrote {}",
                    self.label,
                    describe(&recorded.frame),
                    describe(written)
                )))
            }
        }
        // Learn the alias only for a client-MINTED id (a request, which carries
        // a method). A response's id belongs to the server and needs no alias.
        if method_of(&recorded.frame).is_some() {
            if let (Some(recorded_id), Some(live_id)) =
                (recorded.frame.get("id"), written.get("id"))
            {
                self.aliases.insert(id_key(recorded_id), live_id.clone());
            }
        }
        self.cursor += 1;
        self.sent.push(written.clone());
        Ok(())
    }

    /// Rewrite a recorded RESPONSE's id into the id the pump actually minted.
    ///
    /// Responses only: a server-minted request id is echoed back by the pump
    /// verbatim and must survive untouched.
    fn apply_alias(&self, frame: &mut Value) {
        if method_of(frame).is_some() {
            return;
        }
        if !(frame.get("result").is_some() || frame.get("error").is_some()) {
            return;
        }
        let Some(recorded_id) = frame.get("id") else {
            return;
        };
        let Some(live_id) = self.aliases.get(&id_key(recorded_id)) else {
            return;
        };
        let live_id = live_id.clone();
        if let Some(slot) = frame.get_mut("id") {
            *slot = live_id;
        }
    }
}

impl Transport for TranscriptTransport {
    fn next_line(
        &mut self,
    ) -> impl Future<Output = Result<Option<String>, codex_codes::Error>> + Send {
        ready(self.serve())
    }

    fn send(
        &mut self,
        frame: &Value,
    ) -> impl Future<Output = Result<(), codex_codes::Error>> + Send {
        ready(self.accept(frame))
    }

    /// Always `None`: there is no process. Consequently
    /// [`Session::shutdown`](super::process::Session::shutdown) skips its
    /// `/proc` liveness check, which is correct — that check exists to prove a
    /// real child was reaped, and inventing a pid would make it prove nothing.
    fn pid(&self) -> Option<u32> {
        None
    }

    fn shutdown(self) -> impl Future<Output = Result<(), codex_codes::Error>> + Send {
        ready(Ok(()))
    }
}

/// A transcript mismatch, in the transport's own error vocabulary.
///
/// `Protocol` rather than a new `SessionError` variant: the meaning is exactly
/// "the peer did something the protocol does not allow", and reusing it means
/// the seam cost `SessionError` and `driver.rs`'s error projection nothing.
fn mismatch(detail: String) -> codex_codes::Error {
    codex_codes::Error::Protocol(detail)
}

fn method_of(frame: &Value) -> Option<&str> {
    frame.get("method").and_then(Value::as_str)
}

/// A canonical key for a JSON-RPC id, which the protocol allows to be either an
/// integer or a string.
fn id_key(id: &Value) -> String {
    id.to_string()
}

/// A frame in one short phrase, for a mismatch message that a reader can act
/// on without opening the transcript.
fn describe(frame: &Value) -> String {
    let id = frame.get("id").map(id_key);
    match (method_of(frame), id) {
        (Some(method), Some(id)) => format!("request {method} (recorded id {id})"),
        (Some(method), None) => format!("notification {method}"),
        (None, Some(id)) if frame.get("error").is_some() => format!("error response to id {id}"),
        (None, Some(id)) => format!("response to id {id}"),
        (None, None) => "an unrecognizable frame".to_string(),
    }
}

#[cfg(test)]
mod tests {
    //! Protocol gates: the REAL adapter's turn pump, driven over the REAL
    //! recorded transcript of one live turn.
    //!
    //! Every test here runs the production
    //! [`Session::start_turn`]/[`Session::reply_and_pump`] code — the same
    //! functions [`CodexAgentBackend`](crate::backend::codex::CodexAgentBackend)
    //! calls — against `fixtures/app-server-0.146.0/phase4-live-turn.jsonl`. No
    //! process is spawned, no `~/.codex` is read, and no model is called: the
    //! transport is a file.
    //!
    //! They live inside `backend::codex` rather than in `tests/` because this
    //! crate's containment rule (`lib.rs`, rule 1;
    //! `plans/post-restart/agent-lanes/README.md`) is that `codex-codes` types
    //! appear ONLY under this module — and a gate about protocol frames cannot
    //! be written without them.
    //!
    //! Each test is named for the ONE failure mode it catches.

    use std::path::PathBuf;
    use std::time::Duration;

    use super::*;
    use crate::backend::codex::driver::{project_payload, project_usage};
    use crate::backend::codex::process::{
        last_agent_message_text, Session, SessionError, TurnStop,
    };
    use crate::seam::CycleResultPayload;

    /// Generous, and never actually waited on: the replay transport resolves
    /// every read immediately, so this only bounds a genuine bug (a pump that
    /// loops).
    const TIMEOUT: Duration = Duration::from_secs(5);

    /// The correlation triple and payload as the SERVER sent them in the
    /// recording. Copied from the fixture, never read back out of the adapter —
    /// a gate that sourced its expectations from the code under test would pass
    /// however that code behaved.
    const RECORDED_THREAD_ID: &str = "019fe4de-3248-7182-9ed9-8e8232dfe824";
    const RECORDED_TURN_ID: &str = "019fe4de-3309-75b1-8a4b-41f926ae46f7";
    const RECORDED_CALL_ID: &str = "exec-f115b10a-9448-43d5-8865-bae933ff44c7";
    const RECORDED_PASSPHRASE: &str = "tidepool-cobalt-7-do-not-reuse";
    /// The `item/tool/call` request id the SERVER minted, verbatim from the
    /// recording. Never rewritten — the reply has to carry it back unchanged.
    const RECORDED_SERVER_REQUEST_ID: i64 = 0;

    fn transcript() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/app-server-0.146.0/phase4-live-turn.jsonl")
    }

    /// A session over the recorded turn, positioned so the pump's own
    /// `turn/start` is the next thing the transcript expects.
    fn replayed_turn() -> Session<TranscriptTransport> {
        let transport = TranscriptTransport::from_path(transcript())
            .expect("the committed phase-4 transcript must load")
            .resuming_at("turn/start")
            .expect("the transcript must contain the recorded turn/start");
        Session::over(transport)
    }

    /// The `turn/start` params the pump sends. Deliberately NOT a copy of the
    /// recorded ones: replay matches a client frame by METHOD, because the ids
    /// and the cwd of the recorded run are facts about that run, not about the
    /// protocol.
    fn turn_start_params() -> codex_codes::TurnStartParams {
        codex_codes::TurnStartParams {
            thread_id: RECORDED_THREAD_ID.to_string(),
            input: vec![codex_codes::UserInput::Text {
                text: "replayed".to_string(),
                text_elements: None,
            }],
            ..Default::default()
        }
    }

    fn answered(text: &str) -> codex_codes::DynamicToolCallResponse {
        codex_codes::DynamicToolCallResponse {
            success: true,
            content_items: vec![codex_codes::DynamicToolCallOutputContentItem::InputText {
                text: text.to_string(),
            }],
        }
    }

    /// Drive to the park and hand back the parked call.
    async fn park<T: Transport>(session: &mut Session<T>) -> codex_codes::DynamicToolCallParams {
        match session
            .start_turn(&turn_start_params(), TIMEOUT)
            .await
            .expect("the recorded turn must pump to its tool call")
        {
            TurnStop::ToolCall(params) => params,
            TurnStop::Completed(turn) => panic!(
                "the recorded turn parks on ask_parent before completing; got {:?}",
                turn.status
            ),
        }
    }

    /// GATE: the pump stops at the real recorded `item/tool/call` carrying the
    /// exact correlation triple the server sent, and has written NOTHING back.
    ///
    /// The second half is the one a mock cannot check. A parked call means the
    /// JSON-RPC response is UNWRITTEN, which is precisely what holds the
    /// child's turn open while the parent runs; the only way to see that is to
    /// ask the transport what the pump actually put on the wire.
    #[tokio::test]
    async fn start_turn_parks_at_the_recorded_tool_call_having_written_no_response() {
        let mut session = replayed_turn();
        let call = park(&mut session).await;

        assert_eq!(call.thread_id, RECORDED_THREAD_ID, "recorded threadId");
        assert_eq!(call.turn_id, RECORDED_TURN_ID, "recorded turnId");
        assert_eq!(call.call_id, RECORDED_CALL_ID, "recorded callId");
        assert_eq!(call.tool, "ask_parent");
        assert_eq!(
            call.arguments,
            serde_json::json!({"question": "What is the secret passphrase?"}),
            "the arguments the model actually sent"
        );

        let sent = session.transport().sent();
        assert_eq!(
            sent.len(),
            1,
            "the pump must have written turn/start and nothing else: {sent:?}"
        );
        assert_eq!(sent[0]["method"], serde_json::json!("turn/start"));
        assert!(
            sent.iter().all(|frame| frame.get("result").is_none()),
            "a parked call means NO response has been written: {sent:?}"
        );

        // The session's own view agrees: one observed call, still parked.
        assert_eq!(session.observed_tool_calls().len(), 1);
        assert_eq!(session.observed_tool_calls()[0].call_id, RECORDED_CALL_ID);
    }

    /// GATE: `reply_and_pump` writes the `DynamicToolCallResponse` onto the
    /// parked JSON-RPC id, and the pump then reaches `turn/completed` with
    /// status `Completed`.
    ///
    /// The whole park/reply/resume vertical over recorded frames: the reply
    /// correlates to the SERVER-minted request id, and the recording's
    /// remaining frames are served only because that reply was written.
    #[tokio::test]
    async fn reply_and_pump_writes_the_response_and_reaches_turn_completed() {
        let mut session = replayed_turn();
        let call = park(&mut session).await;

        let stop = session
            .reply_and_pump(&call.call_id, &answered(RECORDED_PASSPHRASE), TIMEOUT)
            .await
            .expect("answering the parked call must resume the recorded turn");

        let sent = session.transport().sent();
        assert_eq!(
            sent.len(),
            2,
            "turn/start then exactly one tool reply: {sent:?}"
        );
        let reply = &sent[1];
        assert_eq!(
            reply["id"],
            serde_json::json!(RECORDED_SERVER_REQUEST_ID),
            "the reply must carry the SERVER-minted request id, verbatim"
        );
        assert_eq!(
            reply["result"]["success"],
            serde_json::json!(true),
            "the protocol's own reply shape (PROTOCOL-NOTES.md §3)"
        );
        assert_eq!(
            reply["result"]["contentItems"][0]["text"],
            serde_json::json!(RECORDED_PASSPHRASE)
        );

        let TurnStop::Completed(turn) = stop else {
            panic!("the recorded turn completes after its single tool call");
        };
        assert!(
            matches!(turn.status, codex_codes::TurnStatus::Completed),
            "recorded turn status: {:?}",
            turn.status
        );
        assert_eq!(turn.id, RECORDED_TURN_ID);
    }

    /// GATE: the terminal `agentMessage` of the recorded turn projects to
    /// [`CycleResultPayload::Structured`].
    ///
    /// Runs the production projection on the `Turn` the pump built from real
    /// frames. The existing unit tests already cover hand-written `Turn`s; this
    /// one proves the projection and the wire agree.
    #[tokio::test]
    async fn the_recorded_terminal_agent_message_projects_to_structured() {
        let mut session = replayed_turn();
        let call = park(&mut session).await;
        let stop = session
            .reply_and_pump(&call.call_id, &answered(RECORDED_PASSPHRASE), TIMEOUT)
            .await
            .expect("resume the recorded turn");
        let TurnStop::Completed(turn) = stop else {
            panic!("expected the recorded completion");
        };

        assert_eq!(
            last_agent_message_text(&turn),
            Some(format!("{{\"result\":\"{RECORDED_PASSPHRASE}\"}}").as_str())
        );
        assert_eq!(
            project_payload(&turn),
            CycleResultPayload::Structured(serde_json::json!({"result": RECORDED_PASSPHRASE}))
        );
    }

    /// GATE: answering a `call_id` that is not the parked one is refused, AND
    /// the parked call is STILL parked afterwards.
    ///
    /// The second clause is the point. A refusal that dropped the parked id
    /// would leave the child's turn hanging until its own timeout with no way
    /// to answer it — the misroute would have destroyed the obligation instead
    /// of reporting it. So this drives the CORRECT reply after the refusal and
    /// requires it to still resume the turn.
    #[tokio::test]
    async fn a_reply_to_the_wrong_call_is_refused_without_losing_the_parked_call() {
        let mut session = replayed_turn();
        let call = park(&mut session).await;

        let error = session
            .reply_and_pump("exec-not-the-parked-call", &answered("wrong"), TIMEOUT)
            .await
            .expect_err("answering a call that is not parked must be refused");
        let SessionError::WrongCall {
            answered: attempted,
            parked,
        } = &error
        else {
            panic!("expected SessionError::WrongCall, got {error:?}");
        };
        assert_eq!(attempted, "exec-not-the-parked-call");
        assert_eq!(parked, RECORDED_CALL_ID);

        assert!(
            session
                .transport()
                .sent()
                .iter()
                .all(|frame| frame.get("result").is_none()),
            "a refused misroute must not reach the wire"
        );

        let stop = session
            .reply_and_pump(&call.call_id, &answered(RECORDED_PASSPHRASE), TIMEOUT)
            .await
            .expect("the parked call survived the refusal and is still answerable");
        assert!(matches!(stop, TurnStop::Completed(_)));
    }

    /// GATE: `reply_and_pump` with nothing parked is
    /// [`SessionError::NoParkedCall`], and writes nothing.
    #[tokio::test]
    async fn reply_and_pump_with_nothing_parked_is_refused() {
        let mut session = replayed_turn();

        let error = session
            .reply_and_pump(RECORDED_CALL_ID, &answered(RECORDED_PASSPHRASE), TIMEOUT)
            .await
            .expect_err("there is no parked call before the turn has started");
        let SessionError::NoParkedCall { call_id } = &error else {
            panic!("expected SessionError::NoParkedCall, got {error:?}");
        };
        assert_eq!(call_id, RECORDED_CALL_ID);
        assert!(
            session.transport().sent().is_empty(),
            "an unmatched reply must not reach the wire"
        );
    }

    /// GATE: the recorded `thread/tokenUsage/updated` frames are captured, and
    /// the LAST one wins.
    ///
    /// The recording carries two, BOTH after the parked call is answered (the
    /// first arrives with the tool result, the second with the terminal
    /// message) — so there is no usage at all at the park, and that is stated
    /// here rather than assumed. They are cumulative per thread, so the pump
    /// must keep the last rather than summing; pinning the second frame's
    /// numbers is what distinguishes "kept the last" from "kept the first" or
    /// "added them", and pinning `last` against the frame's own much larger
    /// `total` (28641) is what distinguishes this turn's cost from the thread's
    /// running total.
    #[tokio::test]
    async fn recorded_token_usage_is_captured_and_the_last_frame_wins() {
        let mut session = replayed_turn();
        let call = park(&mut session).await;

        assert!(
            session.token_usage().is_none(),
            "the recording's first tokenUsage frame arrives only after the tool reply"
        );

        session
            .reply_and_pump(&call.call_id, &answered(RECORDED_PASSPHRASE), TIMEOUT)
            .await
            .expect("resume the recorded turn");

        let projected = project_usage(
            session
                .token_usage()
                .expect("the completed turn must carry usage"),
        );
        assert_eq!(projected.input_tokens, 14326);
        assert_eq!(projected.cached_input_tokens, 14080);
        assert_eq!(projected.output_tokens, 25);
        assert_eq!(projected.reasoning_output_tokens, 0);
        assert_eq!(
            projected.total_tokens, 14351,
            "the LAST tokenUsage frame's `last` (14351), not the first frame's (14290), not the \
             same frame's cumulative `total` (28641)"
        );
    }

    /// GATE: when the recording runs out, the pump reports a clean failure
    /// rather than hanging.
    #[tokio::test]
    async fn an_exhausted_recording_does_not_hang_the_pump() {
        let mut session = replayed_turn();
        let call = park(&mut session).await;
        session
            .reply_and_pump(&call.call_id, &answered(RECORDED_PASSPHRASE), TIMEOUT)
            .await
            .expect("resume the recorded turn");
        assert_eq!(
            session.transport().remaining(),
            0,
            "the whole recorded turn was consumed"
        );

        let error = session
            .start_turn(&turn_start_params(), TIMEOUT)
            .await
            .expect_err("an exhausted recording cannot serve another turn");
        assert!(
            matches!(&error, SessionError::Transport(_)),
            "writing past the end of the recording is a transport mismatch, got {error:?}"
        );
        assert!(
            error.to_string().contains("exhausted"),
            "the failure must say the recording ran out: {error}"
        );
    }

    /// GATE: a read past the end of the recording is a clean `Closed`, not a
    /// hang.
    #[tokio::test]
    async fn a_read_past_the_end_of_the_recording_is_a_clean_closed() {
        let mut transport = TranscriptTransport::from_path(transcript())
            .expect("load the transcript")
            .resuming_at("turn/start")
            .expect("position at turn/start");
        // Drain the whole recording by hand, writing whatever client frame the
        // recording asks for whenever a read reports one is due.
        loop {
            match transport.next_line().await {
                Ok(Some(_)) => continue,
                Ok(None) => break,
                // A read error here means the recording's next frame is the
                // client's; write that exact recorded frame back and continue.
                Err(_) => {
                    let due = transport.frames[transport.cursor].frame.clone();
                    transport
                        .send(&due)
                        .await
                        .expect("replaying the recorded client frame verbatim");
                }
            }
        }
        assert_eq!(transport.remaining(), 0);
        assert_eq!(
            transport
                .next_line()
                .await
                .expect("a clean end, not an error"),
            None,
            "an exhausted recording reads as end-of-stream so the pump reports Closed"
        );
    }

    /// GATE: the WHOLE recorded conversation replays from `initialize`, so the
    /// handshake and `thread/start` go through [`Session::request`]'s own id
    /// correlation over the same recorded bytes.
    ///
    /// This is what exercises the id-alias scheme end to end rather than only
    /// on the turn.
    #[tokio::test]
    async fn the_full_recorded_conversation_replays_from_the_handshake() {
        let transport =
            TranscriptTransport::from_path(transcript()).expect("load the phase-4 transcript");
        let mut session = Session::over(transport);

        let initialize: codex_codes::InitializeResponse = session
            .request(
                codex_codes::methods::INITIALIZE,
                &codex_codes::InitializeParams {
                    client_info: codex_codes::ClientInfo {
                        name: "tidepool-agent".to_string(),
                        title: None,
                        version: "0.1.0".to_string(),
                    },
                    capabilities: Some(codex_codes::InitializeCapabilities {
                        experimental_api: Some(true),
                        ..Default::default()
                    }),
                },
            )
            .await
            .expect("the recorded initialize response");
        assert_eq!(
            initialize.codex_home,
            serde_json::json!("/home/inanna/.codex"),
            "the recorded initialize response, decoded by the production path"
        );

        session
            .notify(codex_codes::methods::INITIALIZED)
            .await
            .expect("the recorded initialized notification");

        let thread: codex_codes::ThreadStartResponse = session
            .request("thread/start", &serde_json::json!({"ephemeral": true}))
            .await
            .expect("the recorded thread/start response");
        assert_eq!(thread.thread.id, RECORDED_THREAD_ID);

        let call = park(&mut session).await;
        assert_eq!(call.call_id, RECORDED_CALL_ID);
        let stop = session
            .reply_and_pump(&call.call_id, &answered(RECORDED_PASSPHRASE), TIMEOUT)
            .await
            .expect("resume the recorded turn");
        assert!(matches!(stop, TurnStop::Completed(_)));

        assert_eq!(
            session.frames().len(),
            35,
            "every frame of the recorded conversation crossed the production pump"
        );
        assert_eq!(session.transport().remaining(), 0);
    }

    /// GATE: the replay transport refuses a frame the recording did not expect,
    /// naming both sides — without this the transport could silently accept
    /// anything and the gates above would prove nothing about ORDER.
    #[tokio::test]
    async fn a_frame_the_recording_did_not_expect_is_refused_by_name() {
        let mut transport = TranscriptTransport::from_path(transcript())
            .expect("load the transcript")
            .resuming_at("turn/start")
            .expect("position at turn/start");
        let error = transport
            .send(&serde_json::json!({"id": 9, "method": "turn/interrupt", "params": {}}))
            .await
            .expect_err("the recording expects turn/start, not turn/interrupt");
        let detail = error.to_string();
        assert!(detail.contains("turn/start"), "{detail}");
        assert!(detail.contains("turn/interrupt"), "{detail}");
    }

    /// GATE: a recorded response's id is rewritten to the id the LIVE pump
    /// minted, so a pump that numbers its requests differently from the
    /// recorded run still correlates.
    ///
    /// Driven by starting the transcript at `thread/start`, which the recording
    /// numbered id 2 while a fresh session numbers it 1.
    #[tokio::test]
    async fn a_recorded_response_id_is_rewritten_to_the_live_pumps_own_id() {
        let transport = TranscriptTransport::from_path(transcript())
            .expect("load the transcript")
            .resuming_at("thread/start")
            .expect("position at thread/start");
        let mut session = Session::over(transport);

        // A fresh session's counter starts at 1; the recording used 2.
        let thread: codex_codes::ThreadStartResponse = session
            .request("thread/start", &serde_json::json!({"ephemeral": true}))
            .await
            .expect("the response must correlate despite the id disagreement");
        assert_eq!(thread.thread.id, RECORDED_THREAD_ID);
        assert_eq!(
            session.transport().sent()[0]["id"],
            serde_json::json!(1),
            "the pump minted 1 where the recording had 2"
        );
    }
}
