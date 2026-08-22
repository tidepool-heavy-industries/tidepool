//! Flinch katas: hard Haskell patterns people reflexively avoid on the JIT,
//! driven through the real compile→JIT→dispatch pipeline via `EvalHarness`.
//!
//! Each kata asserts a concrete RUNTIME result. A failing kata is fixed or
//! ticketed (`#[ignore = "#NNN"]`) — never weakened.
//!
//! Five katas (a, b, c1, c2, c3) are VALUE-class — a regression is a wrong
//! result from a probe that still compiles and returns, so they bundle into
//! one `#[test]` via the check-list idiom
//! (`generic_form_roundtrip.rs`/`jit_surface.rs`). d1 and d2 stay standalone:
//! both pin a call-depth/laziness MECHANISM rather than a value (d1 "forces
//! a deep spine through the eval-stack thread"; d2's failure mode on
//! regression is non-termination, not a wrong value — mirrors
//! `jit_surface.rs`'s own DISTINCT-MECHANISM exclusion class, and a hang or
//! stack blowup mid-probe would take any bundled sibling check down with it.

use serde_json::json;
use tidepool_testing::eval_harness::EvalHarness;

/// Shared pure-module preamble: no effects, just `Tidepool.Prelude` plus the
/// qualified escape hatches the katas need (`P.` for FFI-shadowed numerics).
const PURE_PREAMBLE: &str = r#"{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DeriveGeneric, DeriveAnyClass, FlexibleContexts, FlexibleInstances, ScopedTypeVariables #-}
module Expr where
import Tidepool.Prelude hiding (error)
import qualified Data.Text as T
import qualified Prelude as P
default (Int, Text)
error :: Text -> a
error = P.error . T.unpack
"#;

fn pure_module(body: &str) -> String {
    format!("{PURE_PREAMBLE}\n{body}\n")
}

/// Compile+run `body` (helper defs + the named `target`) as a pure expression
/// (no effects) and return the rendered JSON.
fn run_pure_kata(body: &str, target: &str) -> serde_json::Value {
    EvalHarness::new()
        .with_stdlib()
        .run_pure(&pure_module(body), target)
        .json()
}

/// Value-regression kata family — a regression in any of these is a
/// wrong-but-obtained value, never a crash:
///
///   (a) generic deriving: a record derives `(Generic, FromJSON)`, parsed
///       via the real `eitherDecode`/`fromJSON` pipeline, fields summed —
///       trusting `deriving` to synthesize the parser instead of
///       hand-rolling one.
///   (b) typeclass-in-helpers: a custom class with two instances, called
///       through a constrained polymorphic helper (dictionary passing
///       through a helper function, not just at the call site).
///   (c1) numerics over `[Int]`: exact integer folds.
///   (c2) numerics over `[Double]`: exact folds, rendered via `showDouble`
///        (the JSON literal path is Int-first; Double needs the shadow).
///   (c3) `P.properFraction`/`P.realToFrac` through the qualified escape
///        hatch.
///
/// Absorbed: kata_a_generic_deriving, kata_b_typeclass_in_helpers,
/// kata_c1_numerics_int_folds, kata_c2_numerics_double_folds,
/// kata_c3_properfraction_realtofrac.
#[test]
fn kata_value_regression_family() {
    let json = run_pure_kata(
        r#"data Rec = Rec { rx :: Int, ry :: Int } deriving (Generic, FromJSON)

class Describe a where
  describe :: a -> Text

data Animal = Cat | Dog

instance Describe Animal where
  describe Cat = "cat"
  describe Dog = "dog"

instance Describe Int where
  describe n = pack (show n)

label :: Describe a => a -> Text
label x = "[" <> describe x <> "]"

result :: [Text]
result = concat
  [ check "kata_a_generic_deriving"
      ((case (eitherDecode "{\"rx\":3,\"ry\":4}" :: Either Text Value) of
          Right v -> case (fromJSON v :: Result Rec) of
            Success r -> rx r + ry r
            Error _ -> -1
          Left _ -> -2) == 7)
  , check "kata_b_typeclass_in_helpers"
      ((label Cat <> label Dog <> label (42 :: Int)) == "[cat][dog][42]")
  , check "kata_c1_numerics_int_folds"
      ((sum xs, foldl' (+) 0 xs, product xs) == (15, 15, 120))
  , check "kata_c2_numerics_double_folds"
      ((pack (showDouble (sum ds)) <> "," <> pack (showDouble (foldl' (+) 0 ds)) <> "," <> pack (showDouble (product ds))) == "7.0,7.0,11.25")
  , check "kata_c3_properfraction_realtofrac"
      ((let (i, frac) = P.properFraction (3.75 :: Double) :: (Int, Double)
            back = P.realToFrac (5 :: Int) :: Double
        in pack (show (i :: Int)) <> "," <> pack (showDouble frac) <> "," <> pack (showDouble back)) == "3,0.75,5.0")
  ]
  where
    check nm ok = if ok then [] else [nm]
    xs = [1, 2, 3, 4, 5] :: [Int]
    ds = [1.5, 2.5, 3.0] :: [Double]"#,
        "result",
    );
    assert_eq!(json, json!([]), "failed katas: {json}");
}

/// Kata (d1) — deep strict fold: `foldl' (+) 0 [1..200000 :: Int]` forces a
/// deep spine through the eval-stack thread.
///
/// DISTINCT-MECHANISM — STANDALONE: pins call-depth/eval-stack behavior
/// rather than a stdlib function's ordinary JIT-safety (mirrors
/// `jit_surface.rs`'s own TCO/call-depth exclusion class).
#[test]
fn kata_d1_deep_strict_fold() {
    let json = run_pure_kata(
        r#"result :: Int
result = foldl' (+) 0 [1 .. 200000 :: Int]"#,
        "result",
    );
    eprintln!("kata_d1 result: {json}");
    assert_eq!(json, json!(20_000_100_000i64));
}

/// Kata (d2) — laziness: `take 5` on an infinite list must not force the
/// whole spine.
///
/// HANG-class — STANDALONE: on regression this doesn't return a wrong
/// value, it never returns at all — bundling it would hang the whole probe.
#[test]
fn kata_d2_laziness_infinite_list() {
    let json = run_pure_kata(
        r#"result :: [Int]
result = take 5 [1 :: Int ..]"#,
        "result",
    );
    eprintln!("kata_d2 result: {json}");
    assert_eq!(json, json!([1, 2, 3, 4, 5]));
}
