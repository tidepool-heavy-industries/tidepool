//! Golden round-trip tests for Tidepool.Data.Time: parseISO8601 / formatISO8601.
//! One family bundle, one `tidepool-extract` spawn (`compile_many` — see
//! `plans/test-time-cut.md` §3).
//!
//! Covers:
//!   - format→parse round-trip (epochMillis identity)
//!   - parse→format round-trip (Z-suffix string identity)
//!   - pre-1970 dates (negative epoch-ms)
//!   - git %cI shape with negative UTC offset (e.g. -07:00)
//!   - git %cI shape with positive UTC offset (e.g. +05:30)
//!   - TYPED failure on malformed input (`Left`, not silent corruption)
//!
//! `parseISO8601 :: Text -> Either Text UTCTime` — the parse is a Rust `chrono`
//! primop (ParseISO8601); malformed input is a typed `Left`.

use serde_json::json;
use tidepool_testing::eval_harness::EvalHarness;

const SRC: &str = r#"{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, PartialTypeSignatures #-}
module Test where
import Tidepool.Prelude hiding (error)
import qualified Data.Text as T
default (Int, Text)

check :: Text -> Bool -> [Text]
check nm ok = if ok then [] else [nm]

checks :: [Text]
checks = concat
  [ check "format_then_parse_modern"
      (either (const (-1)) epochMillis (parseISO8601 (formatISO8601 (UTCTime 1709164800000)))
        == 1709164800000)
  , check "parse_then_format_z"
      (either id formatISO8601 (parseISO8601 "2024-02-29T00:00:00Z") == "2024-02-29T00:00:00Z")
  , check "roundtrip_pre1970"
      (either id formatISO8601 (parseISO8601 "1960-03-15T12:00:00Z") == "1960-03-15T12:00:00Z")
  , check "pre1970_epoch_millis_negative"
      (either (const (-1)) epochMillis (parseISO8601 "1960-03-15T12:00:00Z") == (-309182400000))
  , check "parse_git_ci_negative_offset"
      (either id formatISO8601 (parseISO8601 "2026-07-01T19:24:22-07:00")
        == "2026-07-02T02:24:22Z")
  , check "parse_git_ci_positive_offset"
      (either id formatISO8601 (parseISO8601 "2024-01-15T10:30:00+05:30")
        == "2024-01-15T05:00:00Z")
  , check "epoch_zero_roundtrip"
      (either (const (-1)) epochMillis (parseISO8601 (formatISO8601 (UTCTime 0))) == 0)
  , check "malformed_input_is_left"
      (either (const True) (const False) (parseISO8601 "not a timestamp"))
  ]
"#;

#[test]
fn time_module_coverage_family() {
    let h = EvalHarness::new().with_stdlib();
    let artifacts = h
        .compile_many(SRC, &["checks"])
        .expect("compile time_module_coverage family module");
    let out = h.run_target_pure(&artifacts, "checks");
    assert_eq!(
        out.json(),
        json!([]),
        "failed Tidepool.Data.Time checks (see names): {}",
        out.json()
    );
}
