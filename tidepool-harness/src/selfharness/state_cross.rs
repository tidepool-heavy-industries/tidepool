//! `State` crossing at a loop boundary — the SERIALIZED channel across two
//! monads sharing one resident heap, distinct from the in-heap `run_child`
//! channel `service_runllm_hole` uses within a loop. `State` is any
//! author-defined `(ToJSON s, FromJSON s) => s`, so crossing it is NOT a
//! fixed-schema JSON bridge — it reuses the same two mechanisms already
//! proven for other typed/opaque values crossing the Rust/Haskell boundary:
//!
//! - **outbound** (`state_out`): render an evaluated `Value` to
//!   `serde_json::Value` via [`tidepool_runtime::value_to_json`].
//! - **inbound** (`state_in`): splice the prior loop's JSON as a Haskell
//!   literal bound to `__selfHarnessState :: State`, decoded through the
//!   author's `FromJSON State` instance (via `Aeson.eitherDecode`, always in
//!   scope: `import qualified Tidepool.Aeson as Aeson` is in every harness
//!   turn's default preamble) rather than left as a bare `Aeson.Value`.
//!   `None` (the very first loop) splices a reference to the harness's own
//!   `initialState` instead of a decode — there is no prior JSON to decode
//!   yet.
//!
//! The prior compaction summary and the loop-iteration count are NOT
//! spliced into Haskell at all: `driver::SelfHarnessDriver::render_framing`
//! calls the author's `render :: State -> Text` with only the state, then
//! composes those runtime facts onto its `Text` result in Rust (see
//! `plans/self-iterating-harness/15-generic-surface-wave.md`, "Runtime
//! context is the runtime's job").
//!
//! # Why `__selfHarnessState`, not `state`, and `Loaded.` qualification
//!
//! A turn's default preamble always brings `Tidepool.Prelude` into scope
//! UNQUALIFIED, which exports (among other things) a `state` record field
//! (`StatusEntry.state`) and a `render` function (`Tidepool.Render.render`)
//! — the exact names an authored harness module also defines. GHC treats a
//! same-named top-level binding in the SAME compiled turn as an "ambiguous
//! occurrence" against a colliding unqualified import (no automatic local
//! shadowing for top-level names). [`driver::SelfHarnessDriver`] sidesteps
//! this for the harness module's OWN names by importing its decl-plane
//! module QUALIFIED as [`LOADED_QUALIFIER`] (`Loaded.render`, `Loaded.loop`,
//! `Loaded.State`, `Loaded.initialState`) rather than unqualified; this
//! module's own splice avoids it for `state` specifically by using a
//! collision-unlikely name instead.

use serde_json::Value as Json;
use tidepool_eval::value::Value;
use tidepool_repr::DataConTable;

/// The qualifier every harness-source reference (`render`, `loop`, `State`,
/// `initialState`) is imported under — see this module's doc for why
/// qualified, not unqualified.
pub(crate) const LOADED_QUALIFIER: &str = "Loaded";

/// The stable sentinel prefix the [`state_in`] decode splice raises when the
/// prior loop's `State` JSON fails the author's `FromJSON State` instance.
/// The driver matches a fragment run-error carrying this prefix and
/// surfaces it as a typed [`crate::selfharness::driver::DriverError::StateDecode`]
/// rather than an opaque "loop run failed" — a decode failure means the
/// author's `ToJSON`/`FromJSON` are not inverse, which is a distinct,
/// actionable failure mode from a general run error.
pub(crate) const STATE_DECODE_SENTINEL: &str = "TIDEPOOL_STATE_DECODE_FAILED: ";

/// Outbound: `loop`'s returned `State` Haskell value → JSON, via
/// `tidepool_runtime::value_to_json(value, table, 0)`. Called once per loop
/// boundary, after `loop state` completes with a new `State`; the result is
/// what the driver persists (survives a restart) and what the NEXT
/// `render(state)` call receives after being re-spliced by [`state_in`].
pub fn state_out(value: &Value, table: &DataConTable) -> Json {
    tidepool_runtime::value_to_json(value, table, 0)
}

/// Inbound: splice the prior loop's `State` JSON as a Haskell source
/// fragment declaring `__selfHarnessState :: Loaded.State`, decoded via
/// `Aeson.eitherDecode` against the author's `FromJSON State` instance — the
/// source text to prepend to the next `loop`/`render` turn. `None` only for
/// the very first loop, before any `State` has been produced — that case
/// references the harness's own `initialState` instead of decoding
/// anything.
pub fn state_in(state_json: Option<&Json>) -> String {
    match state_json {
        None => format!(
            "__selfHarnessState :: {q}.State\n__selfHarnessState = {q}.initialState\n",
            q = LOADED_QUALIFIER
        ),
        Some(json) => {
            let literal = haskell_string_literal(&json.to_string());
            // On decode failure raise with a stable sentinel prefix the
            // driver recognizes and turns into a typed `DriverError::StateDecode`
            // — a decode failure means the author's `ToJSON`/`FromJSON State`
            // are not inverse, a distinct actionable failure, not a generic run
            // error. `error` is our JIT-safe `Text -> a` (Prelude), reached only
            // on that author-contract violation.
            format!(
                "__selfHarnessState :: {q}.State\n__selfHarnessState = case Aeson.eitherDecode \
                 {literal} of {{ Right s -> s; Left e -> error ({sentinel} <> e) }}\n",
                q = LOADED_QUALIFIER,
                sentinel = haskell_string_literal(STATE_DECODE_SENTINEL),
            )
        }
    }
}

/// Render `s` as a double-quoted Haskell `Text` literal (via
/// `OverloadedStrings`, always on in a harness turn's default pragma set).
/// Used by [`state_in`] for the spliced `State` JSON literal. Reuses the
/// input-lane escaper ([`tidepool_mcp::escape_haskell_string`]) for the body
/// rather than hand-rolling the same escape table, so every generated
/// Haskell string literal (eval `input`, `State`) escapes control chars
/// identically.
pub(crate) fn haskell_string_literal(s: &str) -> String {
    format!("\"{}\"", tidepool_mcp::escape_haskell_string(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_in_none_references_initial_state() {
        assert_eq!(
            state_in(None),
            "__selfHarnessState :: Loaded.State\n__selfHarnessState = Loaded.initialState\n"
        );
    }

    #[test]
    fn state_in_some_splices_an_eitherdecode_literal() {
        let json = serde_json::json!({"loopCount": 3});
        let src = state_in(Some(&json));
        assert!(src.starts_with("__selfHarnessState :: Loaded.State\n"));
        assert!(src.contains("Aeson.eitherDecode"));
        assert!(src.contains("\\\"loopCount\\\":3"));
    }

    #[test]
    fn haskell_string_literal_escapes_quotes_and_backslashes() {
        assert_eq!(haskell_string_literal("a\"b\\c"), "\"a\\\"b\\\\c\"");
    }
}
