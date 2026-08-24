//! `Tidepool.TextFormat` coverage — one family bundle, one `tidepool-extract`
//! spawn (`compile_many` — see `plans/test-time-cut.md` §3).

use serde_json::json;
use tidepool_testing::eval_harness::EvalHarness;

const SRC: &str = r#"{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, PartialTypeSignatures #-}
module Test where
import Tidepool.Prelude
import Tidepool.TextFormat
import qualified Data.Text as T
default (Int, Text)

check :: Text -> Bool -> [Text]
check nm ok = if ok then [] else [nm]

checks :: [Text]
checks = concat
  [ check "camel_to_snake" (camelToSnake "helloWorld" == "hello_world")
  , check "snake_to_camel" (snakeToCamel "hello_world" == "helloWorld")
  , check "capitalize" (capitalize "hello" == "Hello")
  , check "title_case" (titleCase "hello world" == "Hello World")
  , check "slugify" (slugify "Hello World" == "hello-world")
  , check "truncate_text" (truncateText 5 "Hello World" == "He...")
  , check "pad_left" (padLeft 10 "hello" == "     hello")
  , check "pad_right" (padRight 10 "hello" == "hello     ")
  ]
"#;

#[test]
fn text_module_coverage_family() {
    let h = EvalHarness::new().with_stdlib();
    let artifacts = h
        .compile_many(SRC, &["checks"])
        .expect("compile text_module_coverage family module");
    let out = h.run_target_pure(&artifacts, "checks");
    assert_eq!(
        out.json(),
        json!([]),
        "failed Tidepool.TextFormat checks (see names): {}",
        out.json()
    );
}
