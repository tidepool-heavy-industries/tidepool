//! End-to-end: the Haskell `decodeJson :: Text -> Maybe Value` surface compiles
//! through the extractor (Translate.hs lowers it to the `JsonDecode` primop) and
//! runs PURE on the JIT. Requires a worktree extract binary that includes the
//! interception + the stdlib stub (build with `cabal build tidepool-extract-bin`
//! and point `TIDEPOOL_EXTRACT` at it), so it is `#[ignore]` by default.

use serde_json::json;
use tidepool_testing::eval_harness::EvalHarness;

fn run(body: &str) -> serde_json::Value {
    let src = format!(
        r#"{{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, PartialTypeSignatures #-}}
module Test where
import Tidepool.Prelude hiding (error)
import qualified Data.Text as T
default (Int, Text)

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

#[test]
#[ignore = "needs worktree extract binary (TIDEPOOL_EXTRACT) with JsonDecode interception"]
fn decode_scalars() {
    assert_eq!(
        run(r#"case decodeJson "42" of { Just v -> v; Nothing -> Null }"#),
        json!(42)
    );
    assert_eq!(
        run(r#"case decodeJson "true" of { Just v -> v; Nothing -> Null }"#),
        json!(true)
    );
    assert_eq!(
        run(r#"case decodeJson "\"hi\"" of { Just v -> v; Nothing -> Null }"#),
        json!("hi")
    );
    assert_eq!(
        run(r#"case decodeJson "null" of { Just v -> v; Nothing -> Null }"#),
        json!(null)
    );
}

#[test]
#[ignore = "needs worktree extract binary (TIDEPOOL_EXTRACT) with JsonDecode interception"]
fn decode_array_and_object() {
    assert_eq!(
        run(r#"case decodeJson "[1,2,3]" of { Just v -> v; Nothing -> Null }"#),
        json!([1, 2, 3])
    );
    assert_eq!(
        run(r#"case decodeJson "{\"a\":1,\"b\":[true,null]}" of { Just v -> v; Nothing -> Null }"#),
        json!({"a": 1, "b": [true, null]})
    );
}

#[test]
#[ignore = "needs worktree extract binary (TIDEPOOL_EXTRACT) with JsonDecode interception"]
fn decode_malformed_is_nothing() {
    assert_eq!(
        run(
            r#"case decodeJson "{oops" of { Just _ -> String "just"; Nothing -> String "nothing" }"#
        ),
        json!("nothing")
    );
}

#[test]
#[ignore = "needs worktree extract binary (TIDEPOOL_EXTRACT) with JsonDecode interception"]
fn decode_in_pure_fold() {
    // The headline use case: decode each JSONL line inside a pure fold, no effect.
    assert_eq!(
        run(
            r#"map (\l -> case decodeJson l of { Just v -> v; Nothing -> Null }) (T.lines "1\n2\n3")"#
        ),
        json!([1, 2, 3])
    );
}
