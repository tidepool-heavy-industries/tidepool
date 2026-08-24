//! Regression coverage for #312: `T.replace` / `T.breakOn` returning null pointer
//! on multi-line / composite-return inputs.
//!
//! One family bundle, one `tidepool-extract` spawn (`compile_many` — see
//! `plans/test-time-cut.md` §3). Covers the pure JIT path
//! (`compile_and_run_pure`-equivalent). Sister suite in
//! `tidepool-mcp/tests/text_breakon_replace_mcp.rs` covers the effect-dispatch
//! path and is untouched.

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
  [ check "replace_single_line"
      (T.replace "world" "there" "hello world" == "hello there")
  , check "replace_multiline_inline_newlines"
      (T.replace "target" "X" "line one\nline two with target here\nline three"
        == "line one\nline two with X here\nline three")
  , check "replace_with_unlines_body"
      (T.replace "target" "REPLACED"
        (T.unlines [T.pack "line one", T.pack "line two with target here", T.pack "line three"])
        == "line one\nline two with REPLACED here\nline three\n")
  , check "replace_no_match"
      (T.replace "nope" "X" "hello world" == "hello world")
  , check "replace_newline_in_input"
      (T.replace "b" "X" "a\nb\nc" == "a\nX\nc")
  , check "breakon_no_match"
      (let (a, b) = T.breakOn "X" "hello world" in (T.length a, T.length b) == (11, 0))
  , check "breakon_match_at_zero"
      (let (a, b) = T.breakOn "hello" "hello world" in (T.length a, T.length b) == (0, 11))
  , check "breakon_short_needle"
      (let (a, b) = T.breakOn "lo" "hello world" in (T.length a, T.length b) == (3, 8))
  , check "breakon_unlines_body_length_pair"
      (let { body = T.unlines [T.pack "line one", T.pack "line two with target here", T.pack "line three"]
           ; (a, b) = T.breakOn "target" body
           } in (T.length a, T.length b) == (23, 23))
  , check "breakon_just_newline_needle"
      (let (a, b) = T.breakOn "\n" "a\nb" in (T.length a, T.length b) == (1, 2))
  ]
"#;

#[test]
fn text_breakon_replace_pure_family() {
    let h = EvalHarness::new().with_stdlib();
    let artifacts = h
        .compile_many(SRC, &["checks"])
        .expect("compile text_breakon_replace_pure family module");
    let out = h.run_target_pure(&artifacts, "checks");
    assert_eq!(
        out.json(),
        json!([]),
        "failed T.replace/T.breakOn checks (see names): {}",
        out.json()
    );
}
