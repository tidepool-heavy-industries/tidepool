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
//! `TIDEPOOL_EXTRACT` pointed at it, or run inside `nix develop`). Panics
//! loudly (see `require_extract`) when the extractor is unreachable.

use serde_json::json;
use tidepool_testing::eval_harness::{require_extract, EvalHarness};
use tidepool_testing::proptest::{check_jit_vs_eval_captured, CapturedOutcome};

/// Compile + run a full module PURE on the JIT, returning the target binding's
/// JSON. Panics loudly when the extractor is unavailable.
fn run(source: &str, target: &str) -> serde_json::Value {
    require_extract();
    EvalHarness::new()
        .with_stdlib()
        .run_pure(source, target)
        .expect("compile_and_run_pure failed")
        .to_json()
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
    let v = run(&src, "result");
    assert_eq!(v, json!(7), "#337 repro must decode fields summing to 7")
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
    let v = run(&src, "result");
    assert_eq!(v, json!(12), "nested record fields sum to 12")
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
    let v = run(&src, "result");
    assert_eq!(
        v,
        json!({"rx": 3, "ry": 4}),
        "toJSON emits field-keyed object"
    )
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
    let v = run(&src, "result");
    assert_eq!(v, json!(7), "round trip recovers fields summing to 7")
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
    let v = run(&src, "result");
    assert_eq!(
        v,
        json!(0),
        "missing field decodes to Error (0), not a crash"
    )
}

/// A payload sum with RECORD constructors derives BOTH directions through
/// the generic defaults and round-trips losslessly (aeson TaggedObject
/// shape, symmetric encode/decode) — the reject-at-compile-time era is over
/// (design decision, ledger item 14 / structural-cleanup step 3). The value
/// crosses toJSON -> fromJSON on the real JIT and compares equal.
#[test]
fn named_field_sum_round_trips_through_the_generic_defaults() {
    let src = format!(
        "{HEADER}\n\
         data S = A {{ ax :: Int }} | B {{ bt :: Text, bn :: [Int] }} deriving (Generic, ToJSON, FromJSON, Eq)\n\n\
         roundTrip :: S -> Bool\n\
         roundTrip s = case fromJSON (toJSON s) of {{ Success v -> v == s; Error _ -> False }}\n\n\
         result :: Bool\n\
         result = roundTrip (A 7) && roundTrip (B \"hi\" [1, 2, 3])\n"
    );
    assert_eq!(run(&src, "result"), json!(true));
}

/// The encode wire shape for a payload sum is aeson's TaggedObject — tag
/// plus the record fields in ONE object — pinned exactly so a checkpoint
/// written today stays decodable tomorrow.
#[test]
fn named_field_sum_encodes_the_tagged_object_shape() {
    let src = format!(
        "{HEADER}\n\
         data S = A {{ ax :: Int }} | B {{ bt :: Text, bn :: [Int] }} deriving (Generic, ToJSON, FromJSON)\n\n\
         result :: Value\n\
         result = toJSON (B \"hi\" [1, 2])\n"
    );
    assert_eq!(
        run(&src, "result"),
        json!({"tag": "B", "bt": "hi", "bn": [1, 2]})
    );
}

/// A POSITIONAL payload constructor still has no generic ToJSON — there are
/// no field names to key — and the rejection is a compile-time TypeError
/// naming the fix (record syntax), not a runtime surprise.
#[test]
fn positional_sum_tojson_rejected_at_compile_time() {
    require_extract();
    let src = format!(
        "{HEADER}\n\
         data S = A Int | B Int deriving (Generic, ToJSON)\n\n\
         result :: Int\n\
         result = 0\n"
    );
    match EvalHarness::new().with_stdlib().compile(&src, "result") {
        Ok(_) => panic!("positional payload sum deriving ToJSON must not compile"),
        Err(e) => {
            let msg = tidepool_runtime::classify_compile(&e).message;
            assert!(
                msg.contains("record syntax"),
                "expected the positional-payload TypeError, got:\n{msg}"
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
    require_extract();
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
