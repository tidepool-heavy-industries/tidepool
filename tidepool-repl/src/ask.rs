//! Parked-thread Ask suspend/resume for the resident session worker.
//!
//! When an `M a` turn hits the `Ask` effect the [`ReplAskDispatcher`] parks the
//! RESIDENT worker thread on `response_rx` — native stack intact — and emits a
//! [`WorkerMessage::Suspended`]; the server resumes it by sending a
//! [`ResumeMsg`] back, waking the same thread. This is dictated by the repl's
//! model: one long-lived JIT machine whose value heap persists across turns, so
//! the suspension holds the thread rather than tearing it down.
//!
//! This is a DIFFERENT mechanism from the `tidepool` eval server's. That server
//! (`tidepool_runtime::session::engine`, since `ed9588ec`) suspends threadlessly:
//! the JIT codegen effect loop catches the ask tag, the machine is stowed as
//! DATA, the eval thread exits, and resume re-enters the stowed machine on any
//! fresh thread. It has no `DispatchEffect`-level ask dispatcher to share — its
//! `GateDispatcher` does the timeout-yield checkpoint only. Because a resident
//! machine cannot be stowed-and-dropped, the repl keeps the parked-thread
//! mechanism here.
//!
//! What the two DO share is [`tidepool_effect::pause::PauseGate`], the
//! timeout-as-yield-point latch — one gate type, consumed by both dispatchers.
//! The repl worker drives only its abort surface (`request_abort` on timeout,
//! `is_in_effect` at the grace deadline); the gate's pause states + grace
//! machinery go unused here.

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

// The abort latch is the shared [`tidepool_effect::pause::PauseGate`] (unified
// with the eval server's copy — one gate, two dispatchers). The repl worker uses
// only the abort surface: on a turn timeout the server calls `request_abort` and
// the worker unwinds at its next effect dispatch (every effect is a checkpoint);
// a pure JIT stretch that reaches no effect is a runaway and the session is
// marked `Wedged` (the reaper / `session_close` reclaims it). It reads
// `is_in_effect()` at the grace deadline to tell a slow external call
// (Exec/Http/…) from a pure loop. The pause states + grace machinery on the
// shared gate go unused here. Only the gate is shared; the `ReplAskDispatcher`
// and its resident-worker parking stay crate-local.
pub use tidepool_effect::pause::PauseGate;

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
        // A gate abort is a cancellation — record it in the JIT's first-cause
        // cell so the run boundary surfaces `RuntimeError::Cancelled` for a
        // gate-fired abort exactly as it does for a flag-fired cancel.
        // On Ok, `in_effect` is set to true — we must call exit_effect() after.
        self.gate.checkpoint().map_err(|reason| {
            tidepool_codegen::host_fns::set_first_cause(
                tidepool_codegen::host_fns::RuntimeError::Cancelled,
            );
            EffectError::Handler(reason)
        })?;
        let result = self.dispatch_inner(tag, request, cx);
        // Clear in_effect regardless of outcome: the effect handler has returned.
        self.gate.exit_effect();
        result
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

// The gate unit tests (in_effect lifecycle, abort consumption — #324) now live
// with the shared gate in `tidepool_effect::pause`. The ask/suspend integration
// suites in `tests/` exercise this crate's dispatcher wiring around it.
