//! #337: `deriving (Generic, FromJSON)` / `deriving (Generic, ToJSON)` decode and
//! encode single-constructor records generically via GHC.Generics, executed on
//! the JIT through the real extract pipeline (`EvalHarness::run_pure`).
//!
//! Before the fix, `DeriveAnyClass` produced a `FromJSON` instance whose
//! `parseJSON` method was unimplemented (GHC `-Wmissing-methods` warning only) —
//! calling it crashed at runtime with a case trap. The vendored `FromJSON` /
//! `ToJSON` classes now carry `default` methods backed by a `GHC.Generics`
//! representation walk, so the derived instances decode/encode field-name-keyed
//! objects.
//!
//! Requires a worktree extract binary (`cabal build tidepool-extract-bin`, then
//! `TIDEPOOL_EXTRACT` pointed at it, or run inside `nix develop`). Skips cleanly
//! when the extractor is unreachable.

use serde_json::json;
use tidepool_testing::eval_harness::{extract_available, EvalHarness};
use tidepool_testing::proptest::{check_jit_vs_eval_captured, CapturedOutcome};

/// Compile + run a full module PURE on the JIT, returning the target binding's
/// JSON. Skips (returns `None`) when the extractor is unavailable.
fn run(source: &str, target: &str) -> Option<serde_json::Value> {
    if !tidepool_testing::eval_harness::extract_available() {
        eprintln!("skipping: tidepool-extract unavailable (set TIDEPOOL_EXTRACT / nix develop)");
        return None;
    }
    Some(
        EvalHarness::new()
            .with_stdlib()
            .run_pure(source, target)
            .expect("compile_and_run_pure failed")
            .to_json(),
    )
}

const HEADER: &str =
    "{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DeriveGeneric, DeriveAnyClass #-}\n\
                      module Expr where\n\
                      import Tidepool.Prelude hiding (error)\n";

/// The exact repro from issue #337: a two-Int record derives Generic + FromJSON,
/// decodes a field-keyed object, and the fields sum to 7.
#[test]
fn repro_337_fields_sum_to_7() {
    let src = format!(
        "{HEADER}\n\
         data Rec = Rec {{ rx :: Int, ry :: Int }} deriving (Generic, FromJSON)\n\n\
         result :: Int\n\
         result = case (eitherDecode \"{{\\\"rx\\\":3,\\\"ry\\\":4}}\" :: Either Text Value) of\n\
         \x20 Right v -> case (fromJSON v :: Result Rec) of\n\
         \x20   Success r -> rx r + ry r\n\
         \x20   Error _ -> -1\n\
         \x20 Left _ -> -2\n"
    );
    match run(&src, "result") {
        Some(v) => assert_eq!(v, json!(7), "#337 repro must decode fields summing to 7"),
        None => {}
    }
}

/// A record whose field is itself a derived record decodes recursively — the
/// K1 leaf dispatches through the nested type's own generic default.
#[test]
fn nested_record_decodes() {
    let src = format!(
        "{HEADER}\n\
         data Inner = Inner {{ nx :: Int, ny :: Int }} deriving (Generic, FromJSON)\n\
         data Outer = Outer {{ inner :: Inner, oz :: Int }} deriving (Generic, FromJSON)\n\n\
         result :: Int\n\
         result = case (eitherDecode \"{{\\\"inner\\\":{{\\\"nx\\\":3,\\\"ny\\\":4}},\\\"oz\\\":5}}\" :: Either Text Value) of\n\
         \x20 Right v -> case (fromJSON v :: Result Outer) of\n\
         \x20   Success o -> nx (inner o) + ny (inner o) + oz o\n\
         \x20   Error _ -> -1\n\
         \x20 Left _ -> -2\n"
    );
    match run(&src, "result") {
        Some(v) => assert_eq!(v, json!(12), "nested record fields sum to 12"),
        None => {}
    }
}

/// The ToJSON generic default builds a field-name-keyed object.
#[test]
fn generic_tojson_builds_object() {
    let src = format!(
        "{HEADER}\n\
         data Rec = Rec {{ rx :: Int, ry :: Int }} deriving (Generic, ToJSON)\n\n\
         result :: Value\n\
         result = toJSON (Rec 3 4)\n"
    );
    match run(&src, "result") {
        Some(v) => assert_eq!(
            v,
            json!({"rx": 3, "ry": 4}),
            "toJSON emits field-keyed object"
        ),
        None => {}
    }
}

/// Round trip: `fromJSON . toJSON` recovers the record — the acceptance shape.
#[test]
fn round_trip_to_from_json() {
    let src = format!(
        "{HEADER}\n\
         data Rec = Rec {{ rx :: Int, ry :: Int }} deriving (Generic, ToJSON, FromJSON)\n\n\
         result :: Int\n\
         result = case (fromJSON (toJSON (Rec 3 4)) :: Result Rec) of\n\
         \x20 Success r -> rx r + ry r\n\
         \x20 Error _ -> -1\n"
    );
    match run(&src, "result") {
        Some(v) => assert_eq!(v, json!(7), "round trip recovers fields summing to 7"),
        None => {}
    }
}

/// A missing field yields `Error`, not a crash — the decode-failure path returns
/// a value rather than trapping.
#[test]
fn missing_field_returns_error() {
    let src = format!(
        "{HEADER}\n\
         data Rec = Rec {{ rx :: Int, ry :: Int }} deriving (Generic, FromJSON)\n\n\
         result :: Int\n\
         result = case (eitherDecode \"{{\\\"rx\\\":3}}\" :: Either Text Value) of\n\
         \x20 Right v -> case (fromJSON v :: Result Rec) of\n\
         \x20   Success _ -> 1\n\
         \x20   Error _ -> 0\n\
         \x20 Left _ -> -2\n"
    );
    match run(&src, "result") {
        Some(v) => assert_eq!(
            v,
            json!(0),
            "missing field decodes to Error (0), not a crash"
        ),
        None => {}
    }
}

/// A sum type deriving FromJSON is rejected at COMPILE time with a clear
/// message — not a silent DeriveAnyClass-with-missing-method that crashes when
/// called (the original #337 failure mode), and not a raw "no instance" dump.
#[test]
fn sum_type_rejected_at_compile_time() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract unavailable");
        return;
    }
    let src = format!(
        "{HEADER}\n\
         data S = A Int | B Int deriving (Generic, FromJSON)\n\n\
         result :: Int\n\
         result = 0\n"
    );
    match EvalHarness::new().with_stdlib().compile(&src, "result") {
        Ok(_) => panic!("sum type deriving FromJSON must not compile"),
        Err(e) => {
            // `Display` on `CompileError::Diagnostics` is a terse structural
            // summary now (structured spans, not rendered text) — the
            // classifier's message is the joined diagnostic text that
            // actually carries GHC's TypeError.
            let msg = tidepool_runtime::classify_compile(&e).message;
            assert!(
                msg.contains("single-constructor records only"),
                "expected the generic sum-rejection TypeError, got:\n{msg}"
            );
        }
    }
}

/// Differential parity on the #337 decode path: the generic-deriving Core
/// (M1/K1/:*:/selector metadata dictionaries, `eitherDecode` → generic
/// `parseJSON` → field arithmetic) produces the SAME result on the tree-walking
/// eval oracle and the JIT. This is the guarantee that generics-heavy Core is
/// executed consistently by both engines, not just that the JIT returns 7.
///
/// (A narrower shape — `fromJSON . toJSON` FUSED with `+` in one expression —
/// trips an eval-oracle Int# boxing bug where the JIT is correct; that is
/// tracked separately as an engine issue and is not the taught idiom.)
#[test]
fn eval_jit_parity_on_generic_core() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract unavailable");
        return;
    }
    let src = format!(
        "{HEADER}\n\
         data Rec = Rec {{ rx :: Int, ry :: Int }} deriving (Generic, FromJSON)\n\n\
         result :: Int\n\
         result = case (eitherDecode \"{{\\\"rx\\\":3,\\\"ry\\\":4}}\" :: Either Text Value) of\n\
         \x20 Right v -> case (fromJSON v :: Result Rec) of\n\
         \x20   Success r -> rx r + ry r\n\
         \x20   Error _ -> -1\n\
         \x20 Left _ -> -2\n"
    );
    let compiled = EvalHarness::new()
        .with_stdlib()
        .compile(&src, "result")
        .expect("compile failed");
    match check_jit_vs_eval_captured(&compiled.expr, &compiled.table, 64 * 1024) {
        CapturedOutcome::Agree(_) => {}
        other => panic!("eval/JIT disagreed on generic-deriving decode Core: {other:?}"),
    }
}
