//! Effect tracing: a `DispatchEffect` wrapper that records every effect's
//! request/response into a shared buffer as it dispatches. This is the source
//! of the observatory's trace pane — without it, `Event::Effect` is a reserved
//! wire slot nothing writes (see this crate's CLAUDE.md).
//!
//! Purely observational: [`TracingDispatcher`] forwards each dispatch to the
//! inner stack unchanged and captures a JSON snapshot of the request and the
//! response on the way through. `Stream` responses (lazy lists) are recorded as
//! a `"<stream>"` marker rather than force-materialized — tracing must not
//! change effect semantics. The harness drains the buffer after each turn and
//! writes one `Event::Effect` per record (`Harness::flush_effects`).

use std::sync::{Arc, Mutex};

use tidepool_effect::dispatch::{DispatchEffect, EffectContext, Response};
use tidepool_effect::EffectError;
use tidepool_eval::value::Value;
use tidepool_mcp::CapturedOutput;

/// How deep [`tidepool_runtime::value_to_json`] renders a traced value before
/// truncating — bounds the cost of snapshotting a large effect payload.
const TRACE_JSON_DEPTH: usize = 6;

/// One traced effect: which effect (stack `tag`), the request, and the
/// response (or an error / `"<stream>"` marker), captured at dispatch time.
#[derive(Clone, Debug)]
pub struct EffectRecord {
    pub tag: u64,
    pub req: serde_json::Value,
    pub resp: serde_json::Value,
}

/// Shared, append-only buffer of traced effects for one session, drained by
/// the harness after each turn.
pub type EffectTrace = Arc<Mutex<Vec<EffectRecord>>>;

/// Wraps an effect-dispatch stack to record every effect into a shared buffer.
pub struct TracingDispatcher<H> {
    inner: H,
    trace: EffectTrace,
}

impl<H> TracingDispatcher<H> {
    pub fn new(inner: H, trace: EffectTrace) -> Self {
        Self { inner, trace }
    }
}

impl<H: DispatchEffect<CapturedOutput>> DispatchEffect<CapturedOutput> for TracingDispatcher<H> {
    fn dispatch(
        &mut self,
        tag: u64,
        request: &Value,
        cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<Response, EffectError> {
        let req = tidepool_runtime::value_to_json(request, cx.table(), TRACE_JSON_DEPTH);
        let result = self.inner.dispatch(tag, request, cx);
        let resp = match &result {
            Ok(Response::Complete(v)) => tidepool_runtime::value_to_json(v, cx.table(), TRACE_JSON_DEPTH),
            Ok(Response::Stream(_)) => serde_json::json!("<stream>"),
            Err(e) => serde_json::json!({ "error": e.to_string() }),
        };
        if let Ok(mut t) = self.trace.lock() {
            t.push(EffectRecord { tag, req, resp });
        }
        result
    }
}
