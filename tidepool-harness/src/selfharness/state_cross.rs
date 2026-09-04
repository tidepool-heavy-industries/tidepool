//! `State` crossing at a loop boundary — the SERIALIZED channel across two
//! monads sharing one resident heap, distinct from the in-heap finalize
//! channel (bridged `Value`, or a `ValueHandle` for a closure —
//! `Harness::take_finalized_value_keep_open`/`take_live_payload_handle_keep_open`)
//! `service_typed_request_suspension` uses within a loop. `State` is any
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
//! composes those runtime facts onto its `Text` result in Rust — runtime
//! context is the runtime's job, not the authored `render`'s.
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
use tidepool_runtime::session::{assemble_bind_module, TemplateSelector, TurnTemplate};

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

/// [`STATE_DECODE_SENTINEL`]'s sibling for the boot-time resume fold
/// ([`resume_in`]). A DISTINCT prefix, not a shared one: a `ResumeFold` decode
/// failure means the driver's encoder and `Tidepool.Resume`'s hand-written
/// `FromJSON` disagree on the wire contract — a Tidepool bug — whereas a
/// `State` decode failure means the AUTHOR's `ToJSON`/`FromJSON State` are not
/// inverse. Two different people have to fix them, so the driver maps them to
/// two different [`crate::selfharness::driver::DriverError`] variants
/// ([`crate::selfharness::driver::DriverError::ResumeDecode`] and
/// `StateDecode`).
pub(crate) const RESUME_DECODE_SENTINEL: &str = "TIDEPOOL_RESUME_DECODE_FAILED: ";

/// Outbound: `loop`'s returned `State` Haskell value → JSON, via
/// `tidepool_runtime::value_to_json(value, table, 0)`. Called once per loop
/// boundary, after `loop state` completes with a new `State`; the result is
/// what the driver persists (survives a restart) and what the NEXT
/// `render(state)` call receives after being re-spliced by [`state_in`].
pub fn state_out(value: &Value, table: &DataConTable) -> Json {
    tidepool_runtime::value_to_json(value, table, 0)
}

/// Inbound sibling of [`state_in`] for the operator's between-loops message
/// (companion State v2): splice `__operatorMsg :: Maybe Text` so the AUTHORED
/// loop can ingest the message as durable, provenance-tagged memory —
/// neither hidden harness machinery nor model transcription; the ingestion
/// is a reviewable line in the harness source. A harness whose loop ignores
/// `__operatorMsg` compiles with an unused-binding warning at most (the
/// message still reaches the model via the framing line either way).
pub fn operator_msg_in(msg: Option<&str>) -> String {
    match msg {
        None => "__operatorMsg :: Maybe Text
__operatorMsg = Nothing
"
        .to_string(),
        Some(text) => format!(
            "__operatorMsg :: Maybe Text
__operatorMsg = Just {}
",
            haskell_string_literal(text)
        ),
    }
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

/// The ONE stable, never-rotating session `Val` module the fused outer
/// render/loop compile ([`crate::selfharness::driver::SelfHarnessDriver::compile_loop_entry`])
/// injects turn-invariant harness context through, instead of splicing the
/// state/operator-msg JSON as a source literal.
///
/// Reserves `Generation(0)` of the ordinary `Tidepool.Session.Val.G<g>`
/// scheme — no new Haskell-side module-name shape needed. This is safe
/// because a REAL session value bind always mints `val_gen().next()` (>= 1:
/// `PersistentSession::new` starts `val_gen` at `Generation(0)`, doc'd as
/// "the empty session — no Lib/Val module exists yet"), and `set_val_gen`'s
/// monotonic-max rule means repeatedly (re)binding at the fixed
/// `Generation(0)` this module uses never advances that counter — gen 0 can
/// therefore never collide with a real bind on the same session.
pub fn harness_ctx_module() -> tidepool_repr::SessionModule {
    tidepool_repr::SessionModule::val(tidepool_repr::Generation(0))
}

/// The bound name under [`harness_ctx_module`]: `(stateJson, operatorMsgJson)
/// :: (Text, Text)`, both RAW JSON text (never decoded Haskell values) —
/// [`state_in_via_ctx`]/[`operator_msg_in_via_ctx`] decode them, rather than
/// a source literal carrying the state's own bytes, which is what makes the
/// fused outer module's TEXT turn-invariant.
pub const HARNESS_CTX_BINDING: &str = "__harnessCtx";

/// Build the resident-turn statement that (re-)binds [`HARNESS_CTX_BINDING`]
/// at [`harness_ctx_module`]. `state_json` and `operator_msg_json` are raw JSON
/// text; the latter is already a JSON-encoded `Maybe Text`.
///
/// Deliberately its OWN tiny compile, and deliberately never itself
/// memo-cacheable (`tidepool_runtime::cache::invocation_key` treats fresh
/// literal content every call as a cache-hazard shape) — the ONE
/// import (`Data.Text`) keeps it orders of magnitude cheaper than the fused
/// outer module it unblocks from the memo.
///
/// [`SelfHarnessDriver::refresh_harness_ctx`]: crate::selfharness::driver::SelfHarnessDriver::refresh_harness_ctx
pub fn harness_ctx_statement(state_json: &str, operator_msg_json: &str) -> String {
    format!(
        "let {binding} = ({} :: Text, {} :: Text)",
        haskell_string_literal(state_json),
        haskell_string_literal(operator_msg_json),
        binding = HARNESS_CTX_BINDING,
    )
}

/// The one wrapper used to compile [`harness_ctx_statement`] through the
/// shared resident-turn path. The empty effect row is deliberate: refreshing
/// context is a pure value bind, not an authored capability surface.
pub fn harness_ctx_template() -> TurnTemplate {
    let preamble = "{-# LANGUAGE OverloadedStrings, DataKinds, PartialTypeSignatures #-}\n\
                    module TidepoolHarnessCtx where\n\
                    import Data.Text (Text)\n\
                    import Control.Monad.Freer (Eff)\n";
    TurnTemplate {
        kind: TemplateSelector::Bind,
        source: assemble_bind_module(
            preamble,
            "",
            "__result",
            "'[]",
            "{{TURN_STMT}}",
            "{{BINDERS}}",
            false,
        ),
    }
}

/// Fixed helper text for the fused outer compile's `__selfHarnessState`
/// binding, injected-Text sibling of [`state_in`] — IDENTICAL every turn
/// (references [`HARNESS_CTX_BINDING`] only, never the state's own bytes),
/// which is the whole point: the fused outer module's SOURCE never changes
/// turn to turn, so the compile memo hits after the first turn.
///
/// `"null"` (the fresh-boot sentinel [`state_in`]'s `None` arm special-cased
/// in RUST) is now special-cased HERE, in Haskell, since the value crossing
/// the injection plane is always a plain `Text`, never absent — a fresh boot
/// still injects the literal string `"null"` (`harness_ctx_statement`'s
/// caller), and this helper recognizes it the same way `state_in`'s `None`
/// arm used to. Every other value is decoded via the author's `FromJSON
/// State` instance, with the SAME `STATE_DECODE_SENTINEL`-prefixed error on
/// failure as [`state_in`] — the driver's decode-failure handling
/// (`DriverError::StateDecode`) does not need to change.
pub fn state_in_via_ctx() -> String {
    format!(
        "__selfHarnessState :: {q}.State\n__selfHarnessState = case fst {b} of {{ \"null\" -> \
         {q}.initialState; __stateJsonText -> case Aeson.eitherDecode __stateJsonText of {{ \
         Right s -> s; Left e -> error ({sentinel} <> e) }} }}\n",
        q = LOADED_QUALIFIER,
        b = HARNESS_CTX_BINDING,
        sentinel = haskell_string_literal(STATE_DECODE_SENTINEL),
    )
}

/// Injected-Text sibling of [`operator_msg_in`] — see [`state_in_via_ctx`]'s
/// doc for why this is fixed text. The operator message crosses as a
/// JSON-encoded `Maybe Text` (never a bare literal), decoded via the same
/// `Aeson.eitherDecode` machinery every other injected value uses; a decode
/// failure here (which cannot happen from a caller going through
/// [`harness_ctx_statement`], since `serde_json` always emits a value `Maybe
/// Text`'s `FromJSON` accepts) falls back to `Nothing` rather than erroring —
/// the operator message is advisory, not a contract the author's own types
/// pin the way `State` does.
pub fn operator_msg_in_via_ctx() -> String {
    format!(
        "__operatorMsg :: Maybe Text\n__operatorMsg = case Aeson.eitherDecode (snd {b}) of {{ \
         Right m -> m; Left _ -> Nothing }}\n",
        b = HARNESS_CTX_BINDING,
    )
}

/// Inbound sibling of [`state_in`] for the boot-time run-journal FOLD:
/// splice `__selfHarnessResume :: Resume.ResumeFold`, decoded via
/// `Aeson.eitherDecode` against `Tidepool.Resume`'s hand-written `FromJSON`.
/// The driver splices this ONLY when it has a non-empty fold, and pairs it with
/// the wider `Loaded.resumeLoop __selfHarnessResume __selfHarnessState` entry —
/// a fresh boot compiles exactly the `Loaded.loop __selfHarnessState` entry it
/// always did, with no extra helper text at all.
///
/// Deliberately the SAME shape as [`state_in`]'s decode branch, down to the
/// `case … of { Right … ; Left e -> error (SENTINEL <> e) }` form: the sentinel
/// prefix ([`RESUME_DECODE_SENTINEL`]) is what the driver matches on a fragment
/// run-error to raise a typed
/// [`crate::selfharness::driver::DriverError::ResumeDecode`] instead of an
/// opaque "loop run failed".
///
/// `Resume.` is in scope because `Tidepool.Resume` rides `Journal`'s
/// `EffectDecl::extra_imports` (`tidepool_mcp`'s `extra_imports_for!`), and
/// `Journal` is in the outer row unconditionally
/// (`crate::selfharness::driver`'s `outer_decls`) — so the import is present on
/// EVERY outer compile, not only the ones that splice this.
pub fn resume_in(fold: &crate::selfharness::resume::ResumeFold) -> String {
    let literal = haskell_string_literal(&fold.to_json().to_string());
    format!(
        "__selfHarnessResume :: Resume.ResumeFold\n__selfHarnessResume = case \
         Aeson.eitherDecode {literal} of {{ Right f -> f; Left e -> error ({sentinel} <> e) }}\n",
        sentinel = haskell_string_literal(RESUME_DECODE_SENTINEL),
    )
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

    /// Mirrors [`state_in`]'s decode splice exactly — same binding shape, same
    /// `eitherDecode`, same sentinel-on-failure form — differing only in the
    /// bound name, the type, and the sentinel.
    #[test]
    fn resume_in_mirrors_state_in_shape() {
        use crate::selfharness::resume::ResumeFold;
        let fold = ResumeFold::fold(
            "run-1",
            &[tidepool_handlers::JournalEntry {
                ts: 0,
                seq: 4,
                kind: "split".into(),
                key: "branch/a".into(),
                payload: serde_json::json!({"n": 1}),
            }],
        );
        let src = resume_in(&fold);
        assert!(src.starts_with("__selfHarnessResume :: Resume.ResumeFold\n"));
        assert!(src.contains("Aeson.eitherDecode"));
        assert!(src.contains(RESUME_DECODE_SENTINEL));
        assert!(
            src.contains("\\\"runId\\\":\\\"run-1\\\""),
            "the fold's wire json must be spliced as an escaped literal, got: {src}"
        );
        assert!(src.contains("\\\"kind\\\":\\\"split\\\""), "got: {src}");
    }

    /// Byte-determinism: the same fold splices the same text, so a resumed
    /// cycle's compile hits the memo instead of missing on map iteration order.
    #[test]
    fn resume_in_is_byte_deterministic() {
        use crate::selfharness::resume::ResumeFold;
        let entries: Vec<tidepool_handlers::JournalEntry> = ["b", "a", "c"]
            .iter()
            .enumerate()
            .map(|(i, k)| tidepool_handlers::JournalEntry {
                ts: 0,
                seq: i as u64,
                kind: "split".into(),
                key: (*k).to_string(),
                payload: serde_json::json!(i),
            })
            .collect();
        let a = resume_in(&ResumeFold::fold("r", &entries));
        let mut reversed = entries.clone();
        reversed.reverse();
        let b = resume_in(&ResumeFold::fold("r", &reversed));
        assert_eq!(a, b);
    }

    #[test]
    fn haskell_string_literal_escapes_quotes_and_backslashes() {
        assert_eq!(haskell_string_literal("a\"b\\c"), "\"a\\\"b\\\\c\"");
    }

    #[test]
    fn harness_ctx_module_reserves_gen_zero() {
        assert_eq!(
            harness_ctx_module().module_name(),
            "Tidepool.Session.Val.G0"
        );
    }

    #[test]
    fn harness_ctx_turn_embeds_both_literals_in_the_shared_bind_wrapper() {
        let statement = harness_ctx_statement("{\"loopCount\":3}", "null");
        assert_eq!(
            statement,
            "let __harnessCtx = (\"{\\\"loopCount\\\":3}\" :: Text, \"null\" :: Text)"
        );
        let source = tidepool_runtime::session::render_template(
            &harness_ctx_template().source,
            &statement,
            &[HARNESS_CTX_BINDING.to_string()],
        );
        assert!(source.contains("module TidepoolHarnessCtx where"));
        assert!(source.contains("__result = do {"));
        assert!(source.contains("_ <- (pure () :: Eff '[] ())"));
        assert!(source.contains("pure __harnessCtx"));
        assert!(
            !source.contains("__result ::"),
            "the generated wrapper must not reintroduce a warning-producing partial signature"
        );
    }

    /// [`state_in_via_ctx`]/[`operator_msg_in_via_ctx`] take NO arguments and
    /// return the SAME text regardless of what state/operator-msg is live —
    /// the whole point (turn-invariant outer-module text). Not a tautology of
    /// the type signature: this pins the literal bytes so a future edit that
    /// accidentally threads a parameter back in is caught here first.
    #[test]
    fn state_in_via_ctx_is_fixed_text_referencing_harness_ctx() {
        let src = state_in_via_ctx();
        assert_eq!(src, state_in_via_ctx(), "must be turn-invariant");
        assert!(src.starts_with("__selfHarnessState :: Loaded.State\n"));
        assert!(src.contains("fst __harnessCtx"));
        assert!(src.contains("\"null\" -> Loaded.initialState"));
        assert!(src.contains("Aeson.eitherDecode __stateJsonText"));
        assert!(src.contains(STATE_DECODE_SENTINEL));
    }

    #[test]
    fn operator_msg_in_via_ctx_is_fixed_text_referencing_harness_ctx() {
        let src = operator_msg_in_via_ctx();
        assert_eq!(src, operator_msg_in_via_ctx(), "must be turn-invariant");
        assert!(src.starts_with("__operatorMsg :: Maybe Text\n"));
        assert!(src.contains("snd __harnessCtx"));
        assert!(src.contains("Aeson.eitherDecode"));
    }
}
