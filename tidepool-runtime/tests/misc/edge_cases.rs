//! Numeric bounds, Unicode, and empty-list edge cases — one family bundle,
//! one `tidepool-extract` spawn (`compile_many`'s N-in-one-spawn mode; see
//! `plans/test-time-cut.md` §3 and `jit_surface.rs`'s module doc for the
//! check-list idiom this mirrors). `numeric_infinity`/`numeric_nan` stay
//! their own TARGETS (not folded into the boolean check list) because the
//! property under test is the outer Rust JSON render of a non-finite
//! Double (`Infinity`/`NaN` -> JSON `null`), not anything a Haskell-side
//! `==` can observe — the same render-fidelity carve-out `jit_surface.rs`
//! documents for its own non-finite-Double probes.
//!
//! `test_unicode_length` is NOT absorbed: it was already a pre-existing
//! failure at HEAD (`len` is not exported by `Tidepool.Prelude` — "Variable
//! not in scope: len"; the probably-intended function is `T.length`, but
//! fixing that is out of this lane's scope). Kept standalone and unmodified
//! so its own compile failure stays isolated to its own spawn, exactly as
//! before, instead of taking the whole family bundle's compile down with it.

use serde_json::json;
use tidepool_testing::eval_harness::EvalHarness;

fn run_plain(body: &str) -> serde_json::Value {
    let src = format!(
        r#"{{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, PartialTypeSignatures #-}}
module Test where
import Tidepool.Prelude
import qualified Data.Text as T
import Prelude (Bounded(..))

result :: _
result = {body}
"#
    );
    EvalHarness::new()
        .with_stdlib()
        .run_pure(&src, "result")
        .expect("compile_and_run_pure failed")
        .to_json()
}

/// Pre-existing failure at HEAD, unrelated to this lane's consolidation —
/// see the module doc. Left standalone and unmodified.
#[test]
fn test_unicode_length() {
    // "héllo" is 5 CHARACTERS (6 bytes in UTF-8) — length is character count,
    // matching base/text.
    let json = run_plain("len \"héllo\"");
    assert_eq!(json, serde_json::json!(5));
}

const SRC: &str = r#"{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, PartialTypeSignatures #-}
module Test where
import Tidepool.Prelude
import qualified Data.Text as T
import Prelude (Bounded(..))

check :: Text -> Bool -> [Text]
check nm ok = if ok then [] else [nm]

checks :: [Text]
checks = concat
  [ check "numeric_max_bound" ((maxBound :: Int) == 9223372036854775807)
  , check "numeric_min_bound" ((minBound :: Int) == (-9223372036854775808))
  , check "numeric_abs_min_bound" (abs (minBound :: Int) == (minBound :: Int))
  , check "numeric_negate_min_bound" (negate (minBound :: Int) == (minBound :: Int))
  , check "unicode_upper" (T.toUpper "café" == "CAFÉ")
  , check "unicode_reverse" (tReverse "abc" == "cba")
  , check "empty_reverse" (reverse ([] :: [Int]) == [])
  , check "empty_sort" (sort ([] :: [Int]) == [])
  , check "empty_sum" (sum ([] :: [Int]) == 0)
  , check "empty_product" (product ([] :: [Int]) == 1)
  ]

numericInfinity :: Double
numericInfinity = (2 :: Double) ** (1024 :: Double)

numericNan :: Double
numericNan = 0.0 / 0.0
"#;

#[test]
fn misc_edge_cases_family() {
    let h = EvalHarness::new().with_stdlib();
    let artifacts = h
        .compile_many(SRC, &["checks", "numericInfinity", "numericNan"])
        .expect("compile edge_cases family module");

    let checks = h.run_target_pure(&artifacts, "checks");
    assert_eq!(
        checks.json(),
        json!([]),
        "failed edge-case checks (see names): {}",
        checks.json()
    );

    let inf = h.run_target_pure(&artifacts, "numericInfinity");
    assert!(
        inf.json().is_null(),
        "2 ** 1024 :: Double (Infinity) must render as JSON null, got: {}",
        inf.json()
    );

    let nan = h.run_target_pure(&artifacts, "numericNan");
    assert!(
        nan.json().is_null(),
        "0.0 / 0.0 :: Double (NaN) must render as JSON null, got: {}",
        nan.json()
    );
}
