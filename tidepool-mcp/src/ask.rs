//! Ask-effect suspend/resume state machine for the MCP server.
//!
//! The Ask effect turns an eval into a coroutine: when the program hits an
//! `Ask`/`AskWith` constructor the [`AskDispatcher`] parks the eval thread and
//! emits a [`SessionMessage::Suspended`], and the server resumes it by sending a
//! [`ResumeMsg`] back down the channel. [`PauseGate`] implements the orthogonal
//! timeout-as-yield-point mechanism (park at the next effect boundary when a
//! caller's window expires). [`EvalSession`] is the parked-continuation record
//! the server keeps per suspended eval.

use crate::{CapturedOutput, McpEffectHandler};
use std::sync::Arc;
use std::thread::JoinHandle;
use tidepool_bridge::{FromCore, ToCore};
use tidepool_runtime::DispatchEffect;

/// Messages from the eval thread to the MCP server.
pub(crate) enum SessionMessage {
    /// The program hit an Ask effect and is waiting for a response.
    /// `meta` carries AskWith metadata (e.g. a "schema" key) as JSON.
    Suspended {
        prompt: String,
        meta: Option<serde_json::Value>,
    },
    /// The program completed successfully.
    Completed { result: String },
    /// The program encountered an error.
    Error { error: String },
}

/// Messages from the MCP server to the blocked eval thread.
///
/// `Answer` carries the CANONICAL validated JSON value (the validator's
/// parse, not the raw resume text — single source of truth). `Abort`
/// terminates the ask as a handler error.
pub(crate) enum ResumeMsg {
    Answer(serde_json::Value),
    Abort(String),
}

/// What a parked continuation is waiting for — decides resume semantics.
pub(crate) enum SessionKind {
    /// Eval thread is BLOCKED on an Ask: resume validates the reply
    /// against `expected_schema` (if any) and sends it down the channel.
    AwaitingAnswer {
        expected_schema: Option<serde_json::Value>,
    },
    /// Eval thread is PAUSED at an effect boundary (timeout-as-yield):
    /// resume wakes the gate and waits another window (its payload is
    /// ignored — sending on the channel would poison the next ask);
    /// abort wakes the gate with an error.
    Paused,
}

/// The pause gate: timeout-as-yield-point. An eval only computes during
/// an MCP call. When the caller's window expires, the server requests a
/// pause and the eval thread parks itself at its NEXT effect dispatch
/// (we own every dispatch, so every effect is a yield point). Between
/// MCP calls: no compute, no LLM spend, nothing unobserved. Pure JIT
/// stretches can't be interrupted — a thread that reaches no effect
/// within a grace period is treated as a runaway and detached (the old
/// timeout behavior, reserved for exactly that case).
pub(crate) struct PauseGate {
    pub(crate) inner: parking_lot::Mutex<GateInner>,
    pub(crate) cv: parking_lot::Condvar,
}

pub(crate) struct GateInner {
    pub(crate) state: GateState,
    /// True while the thread is inside an effect handler (incl. blocked
    /// on an ask). Used at the grace deadline to distinguish "will park
    /// at the next boundary" from "pure compute runaway".
    pub(crate) in_effect: bool,
}

#[derive(Clone, PartialEq)]
pub(crate) enum GateState {
    Run,
    PauseRequested,
    Paused,
    AbortRequested(String),
}

impl PauseGate {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(PauseGate {
            inner: parking_lot::Mutex::new(GateInner {
                state: GateState::Run,
                in_effect: false,
            }),
            cv: parking_lot::Condvar::new(),
        })
    }

    /// Called by the eval thread at every effect dispatch entry. Parks
    /// while paused; returns Err on abort. On Ok, marks in_effect (the
    /// caller MUST pair with exit_effect).
    pub(crate) fn checkpoint(&self) -> Result<(), String> {
        let mut g = self.inner.lock();
        loop {
            match &g.state {
                GateState::Run => {
                    g.in_effect = true;
                    return Ok(());
                }
                GateState::AbortRequested(r) => {
                    let r = r.clone();
                    g.state = GateState::Run;
                    return Err(r);
                }
                GateState::PauseRequested => {
                    g.state = GateState::Paused;
                    self.cv.notify_all(); // tell the server side we parked
                }
                GateState::Paused => {
                    self.cv.wait(&mut g);
                }
            }
        }
    }

    pub(crate) fn exit_effect(&self) {
        self.inner.lock().in_effect = false;
    }

    pub(crate) fn request_pause(&self) {
        let mut g = self.inner.lock();
        if g.state == GateState::Run {
            g.state = GateState::PauseRequested;
        }
    }

    /// Wake a paused (or pause-pending) thread back into Run.
    pub(crate) fn resume_run(&self) {
        let mut g = self.inner.lock();
        g.state = GateState::Run;
        self.cv.notify_all();
    }

    /// Wake the thread with an abort: its current/next checkpoint
    /// returns Err and the eval terminates as a normal error.
    pub(crate) fn request_abort(&self, reason: String) {
        let mut g = self.inner.lock();
        g.state = GateState::AbortRequested(reason);
        self.cv.notify_all();
    }

    /// Server side, after request_pause: wait up to `grace` for the
    /// thread to park. Returns true if it parked OR is inside an effect
    /// (it will park at the next boundary — long LLM/IO calls must not
    /// be mistaken for runaways); false = pure-compute runaway.
    pub(crate) fn parked_or_in_effect(&self, grace: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + grace;
        let mut g = self.inner.lock();
        loop {
            if g.state == GateState::Paused {
                return true;
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                return g.in_effect;
            }
            self.cv.wait_for(&mut g, deadline - now);
        }
    }
}

/// A suspended evaluation session, waiting for a resume call.
pub(crate) struct EvalSession {
    /// Send a response to unblock the eval thread's Ask handler.
    pub(crate) response_tx: std::sync::mpsc::Sender<ResumeMsg>,
    /// Receive the next message (Completed, Suspended, or Error) from the eval thread.
    pub(crate) session_rx: tokio::sync::mpsc::UnboundedReceiver<SessionMessage>,
    /// The Haskell source code, for error formatting on resume.
    pub(crate) source: Arc<str>,
    /// When this session was created, for eviction ordering. Refreshed on
    /// failed validation so a retrying continuation isn't the eviction
    /// victim while its caller fixes the reply.
    pub(crate) created_at: std::time::Instant,
    /// Output capture for this session.
    pub(crate) captured_output: CapturedOutput,
    /// What this continuation is waiting for.
    pub(crate) kind: SessionKind,
    /// The eval thread's join handle, carried across park/resume cycles so
    /// abort (and crash forensics) can reap the thread.
    pub(crate) thread: Option<JoinHandle<()>>,
    /// The pause gate shared with the eval thread's dispatcher.
    pub(crate) gate: Arc<PauseGate>,
}

/// Wraps an existing effect dispatcher and intercepts the Ask effect tag.
///
/// When the Ask tag is hit, sends a `Suspended` message via the session channel
/// and blocks the current thread until a response arrives.
pub(crate) struct AskDispatcher {
    pub(crate) inner: Box<dyn McpEffectHandler>,
    pub(crate) ask_tag: u64,
    pub(crate) session_tx: tokio::sync::mpsc::UnboundedSender<SessionMessage>,
    pub(crate) response_rx: std::sync::mpsc::Receiver<ResumeMsg>,
    /// Every dispatch entry is a yield point: pause/abort requests from
    /// the server side take effect here.
    pub(crate) gate: Arc<PauseGate>,
}

impl DispatchEffect<CapturedOutput> for AskDispatcher {
    fn dispatch(
        &mut self,
        tag: u64,
        request: &tidepool_eval::value::Value,
        cx: &tidepool_effect::dispatch::EffectContext<'_, CapturedOutput>,
    ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError> {
        // Yield point: park here while paused; error out on abort.
        self.gate
            .checkpoint()
            .map_err(tidepool_effect::error::EffectError::Handler)?;
        let result = self.dispatch_inner(tag, request, cx);
        self.gate.exit_effect();
        result
    }
}

impl AskDispatcher {
    fn dispatch_inner(
        &mut self,
        tag: u64,
        request: &tidepool_eval::value::Value,
        cx: &tidepool_effect::dispatch::EffectContext<'_, CapturedOutput>,
    ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError> {
        if tag == self.ask_tag {
            // Extract prompt (+ AskWith metadata) from the Ask constructor
            let (prompt, meta) = extract_ask_request(request, cx.table())
                .map_err(tidepool_effect::error::EffectError::Handler)?;

            // Signal suspension to the MCP server
            let _ = self
                .session_tx
                .send(SessionMessage::Suspended { prompt, meta });

            // Block until the MCP server sends a response via the resume
            // (or abort) tool. The server side has already JSON-parsed and
            // schema-validated the response — what arrives is canonical.
            let msg = self.response_rx.recv().map_err(|_| {
                tidepool_effect::error::EffectError::Handler(
                    "Ask session closed (timeout or client disconnected)".into(),
                )
            })?;

            match msg {
                ResumeMsg::Answer(json_val) => {
                    let core_val = json_val
                        .to_value(cx.table())
                        .map_err(tidepool_effect::error::EffectError::Bridge)?;
                    Ok(core_val.into())
                }
                ResumeMsg::Abort(reason) => Err(tidepool_effect::error::EffectError::Handler(
                    format!("ask aborted by caller: {reason}"),
                )),
            }
        } else {
            self.inner.dispatch(tag, request, cx)
        }
    }
}

/// Extract the prompt (and optional AskWith metadata) from an Ask request.
///
/// The request is `Con(Ask, [prompt_val])` or `Con(AskWith, [prompt_val,
/// meta_val])`, dispatched by constructor name. Returns an error if the
/// prompt cannot be extracted (e.g., unevaluated closure due to a crash in
/// the string-building expression).
pub(crate) fn extract_ask_request(
    request: &tidepool_eval::value::Value,
    table: &tidepool_repr::DataConTable,
) -> Result<(String, Option<serde_json::Value>), String> {
    use tidepool_eval::value::Value;

    let Value::Con(con_id, fields) = request else {
        return Err(format!(
            "ask received unexpected request shape (expected Con(Ask|AskWith, ..)): {:?}",
            request
        ));
    };

    let con_name = table.name_of(*con_id).unwrap_or("<unknown>");
    // `ask` always suspends via AskWith (carrying the schema); the bare `Ask`
    // constructor was reaped with the structured-Ask collapse.
    let has_meta = match con_name {
        "AskWith" => true,
        other => {
            return Err(format!(
                "ask received unexpected constructor {other:?} (expected AskWith)"
            ))
        }
    };

    let Some(prompt_val) = fields.first() else {
        return Err(format!(
            "ask received unexpected request shape (expected Con({con_name}, ..)): {:?}",
            request
        ));
    };

    // Try using FromCore (handles Text, LitString, [Char])
    let prompt = match String::from_value(prompt_val, table) {
        Ok(s) => s,
        Err(e) => {
            // Provide diagnostic: the prompt text couldn't be extracted,
            // likely because the string-building expression crashed
            // (e.g., unresolved external, partial evaluation).
            return Err(format!(
                "ask prompt could not be evaluated to Text: {e}. \
                 The expression passed to `ask` likely crashed during evaluation \
                 (check for unresolved externals or runtime errors in the prompt string)."
            ));
        }
    };

    let meta = if has_meta {
        // Requests arrive fully forced from the JIT bridge
        // (heap_to_value_forcing), so the aeson Value sub-tree is already
        // materialized — value_to_json renders it directly.
        fields
            .get(1)
            .map(|m| tidepool_runtime::value_to_json(m, table, 0))
    } else {
        None
    };

    Ok((prompt, meta))
}
