//! Flinch katas: hard Haskell patterns people reflexively avoid on the JIT,
//! driven through the real compile→JIT→dispatch pipeline via `EvalHarness`.
//!
//! Each kata asserts a concrete RUNTIME result. A failing kata is fixed or
//! ticketed (`#[ignore = "#NNN"]`) — never weakened.

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

/// Kata (a) — Generic deriving: a record derives `(Generic, FromJSON)`, gets
/// parsed from a JSON literal via the real `decodeJson`/`fromJSON` pipeline,
/// and its fields are summed. This is the capability people flinch from:
/// trusting `deriving` to synthesize the parser instead of hand-rolling one.
#[test]
fn kata_a_generic_deriving() {
    let json = run_pure_kata(
        r#"data Rec = Rec { rx :: Int, ry :: Int } deriving (Generic, FromJSON)

result :: Int
result = case decodeJson "{\"rx\":3,\"ry\":4}" of
  Just v -> case (fromJSON v :: Result Rec) of
    Success r -> rx r + ry r
    Error _ -> -1
  Nothing -> -2"#,
        "result",
    );
    eprintln!("kata_a result: {json}");
    assert_eq!(json, json!(7), "Rec(rx=3, ry=4) should sum to 7, got {json}");
}

/// Kata (b) — typeclass-in-helpers: a custom class with two instances, called
/// through a constrained polymorphic helper (dictionary passing through a
/// helper function, not just at the call site).
#[test]
fn kata_b_typeclass_in_helpers() {
    let json = run_pure_kata(
        r#"class Describe a where
  describe :: a -> Text

data Animal = Cat | Dog

instance Describe Animal where
  describe Cat = "cat"
  describe Dog = "dog"

instance Describe Int where
  describe n = pack (show n)

label :: Describe a => a -> Text
label x = "[" <> describe x <> "]"

result :: Text
result = label Cat <> label Dog <> label (42 :: Int)"#,
        "result",
    );
    eprintln!("kata_b result: {json}");
    assert_eq!(json, json!("[cat][dog][42]"));
}

/// Kata (c1) — numerics over `[Int]`: exact integer folds.
#[test]
fn kata_c1_numerics_int_folds() {
    let json = run_pure_kata(
        r#"result :: (Int, Int, Int)
result = (sum xs, foldl' (+) 0 xs, product xs)
  where xs = [1, 2, 3, 4, 5] :: [Int]"#,
        "result",
    );
    eprintln!("kata_c1 result: {json}");
    assert_eq!(json, json!([15, 15, 120]));
}

/// Kata (c2) — numerics over `[Double]`: exact folds, rendered via
/// `showDouble` (the JSON literal path is Int-first; Double needs the shadow).
#[test]
fn kata_c2_numerics_double_folds() {
    let json = run_pure_kata(
        r#"result :: Text
result = pack (showDouble (sum ds)) <> "," <> pack (showDouble (foldl' (+) 0 ds)) <> "," <> pack (showDouble (product ds))
  where ds = [1.5, 2.5, 3.0] :: [Double]"#,
        "result",
    );
    eprintln!("kata_c2 result: {json}");
    assert_eq!(json, json!("7.0,7.0,11.25"));
}

/// Kata (c3) — `P.properFraction` and `P.realToFrac` through the qualified
/// escape hatch.
#[test]
fn kata_c3_properfraction_realtofrac() {
    let json = run_pure_kata(
        r#"result :: Text
result =
  let (i, frac) = P.properFraction (3.75 :: Double) :: (Int, Double)
      back = P.realToFrac (5 :: Int) :: Double
  in pack (show (i :: Int)) <> "," <> pack (showDouble frac) <> "," <> pack (showDouble back)"#,
        "result",
    );
    eprintln!("kata_c3 result: {json}");
    assert_eq!(json, json!("3,0.75,5.0"));
}

/// Kata (d1) — deep strict fold: `foldl' (+) 0 [1..200000 :: Int]` forces a
/// deep spine through the eval-stack thread.
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

/// Kata (d2) — laziness: `stake 5` on an infinite list must not force the
/// whole spine.
#[test]
fn kata_d2_laziness_infinite_list() {
    let json = run_pure_kata(
        r#"result :: [Int]
result = stake 5 [1 :: Int ..]"#,
        "result",
    );
    eprintln!("kata_d2 result: {json}");
    assert_eq!(json, json!([1, 2, 3, 4, 5]));
}
