//! Regression guard: the case-trap path is GRACEFUL.
//!
//! Component **M** of the tidepool-repl GHCi-style session plan
//! (`plans/ghci-swarm-orchestration.md` §0.4; the "empirical gate" in
//! `plans/ghci-session-persistence.md`). The world-age type story relies on
//! cross-generation type access being a clean *error*, not a process crash: a
//! `case` over a value whose `DataConId` matches no alternative must call the
//! host fn `runtime_case_trap` (sets `RuntimeError::CaseTrap`, returns a poison
//! pointer) and surface as a clean `Err` once `with_signal_protection` returns
//! — NOT a bare Cranelift `trap user2` → `ud2` → SIGILL that takes down the
//! process / MCP connection.
//!
//! ## Why this is the *real* eval path, not bespoke wiring
//!
//! In well-typed pure Haskell a `case` is exhaustive (GHC inserts a `patError`
//! default), so a no-match never arises from `compile_and_run_pure` alone. The
//! realistic way a value reaches a `case` whose alternatives don't include its
//! constructor is exactly the world-age scenario: the value was built
//! *elsewhere* (another generation / another compilation) and arrives at code
//! that doesn't know its constructor. We reproduce that by answering an effect
//! with such a value — the standard, accepted real-path technique already used
//! by `repro_lit_double_case.rs`, `repro_ne_group.rs`, and the effectful tests
//! in `value_case_match.rs`. Everything downstream of the effect boundary —
//! effect-result materialization, the JIT-emitted Case dispatch comparison
//! chain, `runtime_case_trap`, and error surfacing — is production codegen via
//! the real `compile_and_run` entry point. Only the *source* of the mismatched
//! value is a test handler, which is legitimate (it stands in for "a value from
//! another generation").

// The injected reply is a constructed Value; variant names mirror Haskell.
#![allow(clippy::enum_variant_names)]

use std::path::{Path, PathBuf};

use tidepool_bridge_derive::FromCore;
use tidepool_effect::{EffectContext, EffectError, EffectHandler, Response};
use tidepool_repr::DataConId;
use tidepool_runtime::{compile_and_run, compile_and_run_pure, Value};

fn prelude_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("haskell")
        .join("lib")
}

/// A `DataConId` that belongs to no constructor of any type referenced below.
/// Real ids are content-addressed `fingerprint("module:conname")` hashes
/// (`Translate.hs:1466`), so a hand-picked synthetic constant cannot collide.
/// Stands in for "a constructor from a generation the consuming code never saw."
const ALIEN_CON_ID: u64 = 0xDEAD_BEEF_CA5E_57A1;

/// What a `Probe` effect should answer with.
enum Reply {
    /// Bridge a JSON value through the same path the live server uses — yields a
    /// *real*, correctly-tagged aeson `Value` constructor.
    Json(serde_json::Value),
    /// A pre-built core `Value` injected verbatim (e.g. a `Con` whose
    /// `DataConId` matches none of the consuming code's alternatives).
    Raw(Value),
}

#[derive(FromCore)]
#[allow(dead_code)] // payload consumed by the FromCore derive, not by Rust.
enum ProbeReq {
    #[core(name = "Probe")]
    Probe(String),
}

/// Answers the single `Probe` effect with a caller-chosen reply.
struct ProbeDispatcher {
    reply: Reply,
}

impl EffectHandler for ProbeDispatcher {
    type Request = ProbeReq;
    fn handle(&mut self, _req: ProbeReq, cx: &EffectContext) -> Result<Response, EffectError> {
        match &self.reply {
            Reply::Json(j) => cx.respond(j.clone()),
            Reply::Raw(v) => Ok(v.clone().into()),
        }
    }
}

/// Drive the REAL eval entry point (`compile_and_run`) on its own signal-handled
/// thread, answering `Probe` with `reply`. Returns the rendered JSON on success
/// or the *stringified* error on failure. A hard crash (uncaught SIGILL/SIGSEGV
/// unwinding the thread, or a panic) becomes `Err("thread panicked ...")` rather
/// than killing the test process — so an ungraceful trap is observable as a
/// failed assertion, not a vanished test binary.
fn drive(src: &str, target: &str, reply: Reply) -> Result<serde_json::Value, String> {
    let src = src.to_owned();
    let target = target.to_owned();
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            tidepool_codegen::signal_safety::install();
            let pp = prelude_path();
            let include = [pp.as_path()];
            let mut handlers = frunk::hlist![ProbeDispatcher { reply }];
            match compile_and_run(&src, &target, &include, &mut handlers, &()) {
                Ok(v) => Ok(v.to_json()),
                Err(e) => Err(format!("{e}")),
            }
        })
        .unwrap()
        .join()
        .map_err(|_| "thread panicked (HARD crash / uncaught signal)".to_string())?
}

/// Source: classify an aeson `Value` over ALL SIX of its constructors with no
/// wildcard, so GHC emits an exhaustive `case` with NO default alternative. A
/// value whose `DataConId` matches none of the six therefore falls straight
/// through to `runtime_case_trap`.
const CLASSIFY_VALUE_SRC: &str = r#"
{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, FlexibleContexts, FlexibleInstances, GADTs, PartialTypeSignatures, ScopedTypeVariables #-}
module Expr where
import Tidepool.Prelude hiding (error)
import qualified Data.Text as T
import Control.Monad.Freer hiding (run)
import qualified Prelude as P
default (Int, Text)
error :: Text -> a
error = P.error . T.unpack

data Probe a where
  Probe :: Text -> Probe Value

type M = Eff '[Probe]

-- Exhaustive over Object/Array/String/Number/Bool/Null → no default branch.
classify :: Value -> Int
classify v = case v of
  Object _ -> 1
  Array _  -> 2
  String _ -> 3
  Number _ -> 4
  Bool _   -> 5
  Null     -> 6

result :: M Int
result = do
  v <- send (Probe "x")
  pure (classify v)
"#;

/// THE GUARD: a `case` over a value whose constructor matches no alternative
/// yields a clean `CaseTrap` error, and the process survives.
#[test]
fn graceful_case_trap_surfaces_as_clean_error() {
    let r = drive(
        CLASSIFY_VALUE_SRC,
        "result",
        // A constructor from "another generation": matches none of the six.
        Reply::Raw(Value::Con(DataConId(ALIEN_CON_ID), vec![])),
    );

    let err = r.expect_err(
        "a no-match case must FAIL gracefully, not return a value — got Ok, \
         which means the alien constructor silently matched an alternative",
    );

    // Graceful: the host case-trap path fired (clean error message).
    assert!(
        err.contains("case trap"),
        "expected a graceful CaseTrap error, got: {err}"
    );

    // NOT a crash: a bare Cranelift trap would surface as a JIT signal
    // (SIGILL/SIGTRAP) or unwind the thread (\"thread panicked\"). Either of
    // those means the trap was NOT graceful — the whole point of the guard.
    assert!(
        !err.contains("signal") && !err.contains("SIGILL") && !err.contains("SIGTRAP"),
        "case trap must be graceful, but it surfaced as a fatal signal: {err}"
    );
    assert!(
        !err.contains("thread panicked"),
        "case trap took down the eval thread (hard crash): {err}"
    );
}

/// Positive control: the exact same source + harness, answered with a *real*
/// `Null` (correctly-tagged via the JSON bridge), classifies cleanly to 6. This
/// proves the harness actually reaches `classify` and that a matching
/// constructor dispatches — so the failure above is specifically the no-match,
/// not a broken test rig.
#[test]
fn matching_constructor_classifies_cleanly() {
    let r = drive(
        CLASSIFY_VALUE_SRC,
        "result",
        Reply::Json(serde_json::json!(null)),
    );
    assert_eq!(
        r.ok(),
        Some(serde_json::json!(6)),
        "a real Null value must match the Null alternative (= 6)"
    );
}

// ---------------------------------------------------------------------------
// Wave 3 scaffold (ignored until session value-binding lands)
// ---------------------------------------------------------------------------

/// Generation 1 of a session type. `chosen` is a value of the original shape.
const GEN1_RESHAPE_SRC: &str = r#"
{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}
module Session where
import Tidepool.Prelude hiding (error)

-- Original constructor set; `Square` exists in this generation.
data Shape = Circle | Square | Triangle

chosen :: Shape
chosen = Square
"#;

/// Generation 3 of the SAME session module name (so unchanged constructors keep
/// stable content-addressed ids), but `Shape` has been RESHAPED: `Square` is
/// gone. Code here cases on `Shape` exhaustively over its *current* constructors
/// — so a persisted gen-1 `Square` value (id `fingerprint("Session:Square")`)
/// matches no alternative.
const GEN3_RESHAPE_SRC: &str = r#"
{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, FlexibleContexts, FlexibleInstances, GADTs, PartialTypeSignatures, ScopedTypeVariables #-}
module Session where
import Tidepool.Prelude hiding (error)
import qualified Data.Text as T
import Control.Monad.Freer hiding (run)
import qualified Prelude as P
default (Int, Text)
error :: Text -> a
error = P.error . T.unpack

-- RESHAPED: `Square` removed between generations.
data Shape = Circle | Triangle

data Probe a where
  Probe :: Text -> Probe Shape

type M = Eff '[Probe]

describe :: Shape -> Int
describe s = case s of
  Circle   -> 1
  Triangle -> 2

result :: M Int
result = do
  s <- send (Probe "x")
  pure (describe s)
"#;

/// SCAFFOLD for Wave 3 (value-binding across turns). Confirms that reshaping a
/// constructor away between two "turns" makes an old value's `case` a graceful
/// no-match, not a SIGILL — the reorder/reshape-safety property the world-age
/// design leans on.
///
/// Ignored because the genuine cross-turn carry — binding `chosen` in one
/// session turn and slicing it in a later, recompiled turn — runs through the
/// session entry point (`session_open`/`session_eval`) + the binding table,
/// which do not exist until Wave 3. Until then this body *simulates* the carry
/// by extracting the real gen-1 `Square` value (it carries its true
/// `DataConId`) and injecting it into gen-3's reshaped `case` via the effect
/// boundary. When Wave 3 lands, replace the extract+inject with two real
/// session turns; the assertion (graceful no-match, never a fatal signal) is
/// the part that must hold and stays unchanged.
#[test]
#[ignore = "Wave 3: cross-turn value carry needs session_open/session_eval + binding table; \
            until then the carry is simulated by extract+inject. Flip on once value-binding lands."]
fn reshape_across_turns_is_graceful_no_match() {
    let result = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || -> Result<serde_json::Value, String> {
            tidepool_codegen::signal_safety::install();
            let pp = prelude_path();
            let include = [pp.as_path()];

            // --- "Turn 1": bind a value of the original shape. ---
            // Its heap `Con` carries the real fingerprint("Session:Square").
            let gen1 = compile_and_run_pure(GEN1_RESHAPE_SRC, "chosen", &include)
                .map_err(|e| format!("gen-1 bind failed: {e}"))?;
            let carried: Value = gen1.into_value();

            // --- "Turn 3": recompiled module where `Square` was reshaped away.
            // The carried gen-1 value reaches gen-3's exhaustive `case Shape`. ---
            let mut handlers = frunk::hlist![ProbeDispatcher {
                reply: Reply::Raw(carried),
            }];
            match compile_and_run(GEN3_RESHAPE_SRC, "result", &include, &mut handlers, &()) {
                Ok(v) => Ok(v.to_json()),
                Err(e) => Err(format!("{e}")),
            }
        })
        .unwrap()
        .join()
        .map_err(|_| "thread panicked (HARD crash / uncaught signal)".to_string())
        .and_then(|inner| inner);

    let err = result.expect_err(
        "a reshaped-away constructor must produce a no-match error in the new \
         generation, not silently match",
    );
    assert!(
        err.contains("case trap"),
        "cross-generation no-match must be a graceful CaseTrap, got: {err}"
    );
    assert!(
        !err.contains("signal") && !err.contains("SIGILL") && !err.contains("SIGTRAP"),
        "reshape no-match must be graceful, but surfaced as a fatal signal: {err}"
    );
}
