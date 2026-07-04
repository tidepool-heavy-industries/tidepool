//! #339: the tree-walking eval oracle diverged from the JIT on
//! `fromJSON . toJSON` FUSED with arithmetic in one expression — the eval
//! oracle raised `TypeMismatch { expected: "Int#", .. }` on a boxed `I#`
//! while the JIT correctly returned 7.
//!
//! Root cause: GHC's strict-field unboxing (`-funbox-small-strict-fields`,
//! implied at -O1+) leaves the vendored `Aeson` `Value`'s `NumberI !Int`
//! constructor's field genuinely unboxed at pattern-match sites, so a
//! generic-deriving decode Core can rebox an operand one layer deeper than
//! the immediately enclosing `case`-of already unwrapped. The oracle's scalar
//! primop extractors only stripped exactly one `Con` layer; the fix (in
//! `tidepool-eval/src/eval.rs`) makes them peel repeated single-field boxed
//! layers, matching the JIT's tolerance for the same shape.
use tidepool_testing::eval_harness::{extract_available, EvalHarness};
use tidepool_testing::proptest::{check_jit_vs_eval_captured, CapturedOutcome};

const HEADER: &str =
    "{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DeriveGeneric, DeriveAnyClass #-}\n\
                      module Expr where\n\
                      import Tidepool.Prelude hiding (error)\n";

/// The exact #339 minimal repro: `fromJSON (toJSON (Rec 3 4))` fused with `+`
/// in one expression. Both engines must return 7.
#[test]
fn repro_339_eval_jit_parity_on_fused_round_trip() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract unavailable (set TIDEPOOL_EXTRACT / nix develop)");
        return;
    }
    let src = format!(
        "{HEADER}\n\
         data Rec = Rec {{ rx :: Int, ry :: Int }} deriving (Generic, ToJSON, FromJSON)\n\n\
         result :: Int\n\
         result = case (fromJSON (toJSON (Rec 3 4)) :: Result Rec) of\n\
         \x20 Success r -> rx r + ry r\n\
         \x20 Error _ -> -1\n"
    );
    let compiled = EvalHarness::new()
        .with_stdlib()
        .compile(&src, "result")
        .expect("compile failed");

    match check_jit_vs_eval_captured(&compiled.expr, &compiled.table, 64 * 1024) {
        CapturedOutcome::Agree(tidepool_eval::value::Value::Con(_, ref fields))
            if matches!(
                fields.as_slice(),
                [tidepool_eval::value::Value::Lit(
                    tidepool_repr::Literal::LitInt(7)
                )]
            ) => {}
        other => {
            panic!("eval/JIT disagreed on #339 fused round-trip Core (expected boxed 7): {other:?}")
        }
    }
}
