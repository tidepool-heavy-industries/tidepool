//! WS-C seam: `State` crossing at a loop boundary — the SERIALIZED channel
//! (02-runtime.md "Two monads over one resident heap"), distinct from the
//! in-heap `run_child` channel `service_runllm_hole` uses within a loop.
//! `State` is any author-defined `(ToJSON s, FromJSON s) => s` (LOCKED,
//! 02-runtime.md), so crossing it is NOT a fixed-schema JSON bridge — it
//! reuses the same two mechanisms already proven for other typed/opaque
//! values crossing the Rust/Haskell boundary:
//!
//! - **outbound** (`state_out`): render an evaluated `Value` to
//!   `serde_json::Value` via [`tidepool_runtime::value_to_json`] — the same
//!   function `crate::engine::decode_askwith` already uses to pull an
//!   `AskWith` payload out of a suspended request.
//! - **inbound** (`state_in`): splice the prior loop's JSON as a Haskell
//!   literal bound to `state :: State`, mirroring
//!   `tidepool_mcp::eval_prep::input_binding_source`'s `input :: Aeson.Value`
//!   splice — except decoded through the author's `FromJSON State` instance
//!   (via `eitherDecode`) rather than left as a bare `Aeson.Value`.

use serde_json::Value as Json;
use tidepool_eval::value::Value;
use tidepool_repr::DataConTable;

/// Outbound: `loop`'s returned `State` Haskell value → JSON, via
/// `tidepool_runtime::value_to_json(value, table, 0)`. Called once per loop
/// boundary, after `loop state` completes with a new `State`; the result is
/// what the driver persists (survives a restart) and what the NEXT
/// `render(state, lastCompaction)` call receives after being re-spliced by
/// [`state_in`].
pub fn state_out(value: &Value, table: &DataConTable) -> Json {
    let _ = (value, table);
    unimplemented!(
        "WS-C: tidepool_runtime::value_to_json(value, table, 0) — outbound State \
         crossing at a loop boundary"
    )
}

/// Inbound: splice the prior loop's `State` JSON as a Haskell source
/// fragment declaring `state :: State`, decoded via `eitherDecode` against
/// the author's `FromJSON State` instance — the source text to prepend to
/// the next `loop`/`render` turn (mirrors
/// `tidepool_mcp::eval_prep::input_binding_source`'s splice shape, targeting
/// a typed `State` rather than a bare `Aeson.Value`). `None` only for the
/// very first loop, before any `State` has been produced (mirrors
/// `render`'s `Maybe Text` compaction argument being `Nothing` pre-history).
pub fn state_in(state_json: Option<&Json>) -> String {
    let _ = state_json;
    unimplemented!(
        "WS-C: splice state_json as a `state :: State` literal, decoded via eitherDecode"
    )
}
