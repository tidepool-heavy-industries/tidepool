//! Parked-thread Ask suspend/resume for the resident session worker.
//!
//! This is the same mechanism the `tidepool` eval server uses (`tidepool-mcp`'s
//! `ask.rs`), reused here against the RESIDENT worker thread instead of a
//! spawned-per-eval one: when an `M a` turn hits the `Ask` effect the
//! [`ReplAskDispatcher`] parks the worker thread on `response_rx` and emits a
//! [`WorkerMessage::Suspended`]; the server resumes it by sending a
//! [`ResumeMsg`] back. [`PauseGate`] is the orthogonal timeout-as-yield-point
//! latch (park at the next effect boundary when a turn's window expires).
//!
//! The eval server's `ask.rs` items are `pub(crate)`, so rather than widen its
//! visibility (and couple to its struct layout) the small mechanism is mirrored
//! here. `tidepool-mcp` is left untouched.

use std::sync::Arc;

use tidepool_bridge::{FromCore, ToCore};
use tidepool_effect::dispatch::EffectContext;
use tidepool_effect::error::EffectError;
use tidepool_effect::Response;
use tidepool_eval::value::Value;
use tidepool_mcp::CapturedOutput;
use tidepool_repr::DataConTable;

/// Messages from the worker thread to the async server, per turn.
pub enum WorkerMessage {
    /// The turn hit an `Ask` effect and is blocked for a response.
    Suspended {
        prompt: String,
        meta: Option<serde_json::Value>,
    },
    /// The turn completed — the rendered result string.
    Completed { result: String },
    /// The turn failed.
    Error { error: String },
    /// `session_close` finished: the resident machine was dropped.
    Closed,
}

/// Messages from the async server back to the blocked worker thread.
pub enum ResumeMsg {
    /// The canonical (already-validated) JSON answer to an `Ask`.
    Answer(serde_json::Value),
    /// Abort the ask as a handler error.
    Abort(String),
}

/// Abort latch. A turn only computes during an MCP call; when the call's window
/// expires (timeout), the server requests an abort and the worker unwinds at its
/// next effect dispatch (every effect is a checkpoint). A pure JIT stretch that
/// reaches no effect can't be interrupted — that turn is a runaway and the
/// session is marked `Wedged` (the reaper / `session_close` reclaims it).
pub struct PauseGate {
    state: parking_lot::Mutex<GateState>,
}

#[derive(Clone, PartialEq)]
enum GateState {
    Run,
    AbortRequested(String),
}

impl PauseGate {
    pub fn new() -> Arc<Self> {
        Arc::new(PauseGate {
            state: parking_lot::Mutex::new(GateState::Run),
        })
    }

    /// Worker side, at every effect dispatch entry: return `Err(reason)` if an
    /// abort was requested (the turn then unwinds), else `Ok` to proceed.
    pub fn checkpoint(&self) -> Result<(), String> {
        let mut g = self.state.lock();
        if let GateState::AbortRequested(r) = &*g {
            let r = r.clone();
            *g = GateState::Run;
            return Err(r);
        }
        Ok(())
    }

    /// Server side (on turn timeout): ask the worker to unwind at its next effect.
    pub fn request_abort(&self, reason: String) {
        *self.state.lock() = GateState::AbortRequested(reason);
    }
}

/// Wraps the session's base effect handler stack and intercepts the `Ask` tag.
///
/// Built fresh per turn from the per-turn channels. When the `Ask` tag fires it
/// emits [`WorkerMessage::Suspended`] and blocks the worker thread on
/// `response_rx` until the server resumes (or aborts) it.
pub struct ReplAskDispatcher<H> {
    pub inner: H,
    pub ask_tag: u64,
    pub session_tx: tokio::sync::mpsc::UnboundedSender<WorkerMessage>,
    pub response_rx: std::sync::mpsc::Receiver<ResumeMsg>,
    pub gate: Arc<PauseGate>,
}

impl<H: tidepool_effect::dispatch::DispatchEffect<CapturedOutput>>
    tidepool_effect::dispatch::DispatchEffect<CapturedOutput> for ReplAskDispatcher<H>
{
    fn dispatch(
        &mut self,
        tag: u64,
        request: &Value,
        cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<Response, EffectError> {
        // Checkpoint: unwind here (Err) if the turn was aborted (timeout).
        self.gate.checkpoint().map_err(EffectError::Handler)?;
        self.dispatch_inner(tag, request, cx)
    }
}

impl<H: tidepool_effect::dispatch::DispatchEffect<CapturedOutput>> ReplAskDispatcher<H> {
    fn dispatch_inner(
        &mut self,
        tag: u64,
        request: &Value,
        cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<Response, EffectError> {
        if tag == self.ask_tag {
            let (prompt, meta) =
                extract_ask_request(request, cx.table()).map_err(EffectError::Handler)?;
            let _ = self
                .session_tx
                .send(WorkerMessage::Suspended { prompt, meta });
            let msg = self.response_rx.recv().map_err(|_| {
                EffectError::Handler("Ask session closed (timeout or client disconnected)".into())
            })?;
            match msg {
                ResumeMsg::Answer(json_val) => {
                    let core_val = json_val.to_value(cx.table()).map_err(EffectError::Bridge)?;
                    Ok(core_val.into())
                }
                ResumeMsg::Abort(reason) => Err(EffectError::Handler(format!(
                    "ask aborted by caller: {reason}"
                ))),
            }
        } else {
            self.inner.dispatch(tag, request, cx)
        }
    }
}

/// Extract the prompt (and optional `AskWith` metadata) from an `Ask` request.
/// Mirrors `tidepool-mcp::ask::extract_ask_request`.
pub fn extract_ask_request(
    request: &Value,
    table: &DataConTable,
) -> Result<(String, Option<serde_json::Value>), String> {
    let Value::Con(con_id, fields) = request else {
        return Err(format!(
            "ask received unexpected request shape (expected Con(AskWith, ..)): {request:?}"
        ));
    };
    let con_name = table.name_of(*con_id).unwrap_or("<unknown>");
    if con_name != "AskWith" {
        return Err(format!(
            "ask received unexpected constructor {con_name:?} (expected AskWith)"
        ));
    }
    let Some(prompt_val) = fields.first() else {
        return Err(format!(
            "ask received unexpected request shape (expected Con(AskWith, ..)): {request:?}"
        ));
    };
    let prompt = String::from_value(prompt_val, table).map_err(|e| {
        format!("ask prompt could not be evaluated to Text: {e}. The expression passed to `ask` likely crashed during evaluation.")
    })?;
    let meta = fields
        .get(1)
        .map(|m| tidepool_runtime::value_to_json(m, table, 0));
    Ok((prompt, meta))
}
