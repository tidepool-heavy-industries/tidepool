//! End-to-end: the Haskell pure JSON-decode surface — `eitherDecode :: FromJSON a
//! => Text -> Either Text a` — compiles through the extractor (Translate.hs lowers
//! the OPAQUE `eitherDecodeValue` primop anchor to the `JsonDecode` primop) and
//! runs PURE on the JIT: no effect, no abort. `eitherDecode @Value` is the raw
//! parse (identity `FromJSON` instance); malformed input is `Left <serde error>`,
//! not `Nothing`. Requires a worktree extract binary that includes the
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

// The result type var is annotated `:: Either Text Value` throughout: a bare
// `eitherDecode` whose decoded value is discarded is ambiguous, exactly as in
// upstream aeson. `Value` selects the identity `FromJSON` instance (raw parse).

#[test]
#[ignore = "needs worktree extract binary (TIDEPOOL_EXTRACT) with JsonDecode interception"]
fn decode_scalars() {
    assert_eq!(
        run(r#"case (eitherDecode "42" :: Either Text Value) of { Right v -> v; Left _ -> Null }"#),
        json!(42)
    );
    assert_eq!(
        run(r#"case (eitherDecode "true" :: Either Text Value) of { Right v -> v; Left _ -> Null }"#),
        json!(true)
    );
    assert_eq!(
        run(r#"case (eitherDecode "\"hi\"" :: Either Text Value) of { Right v -> v; Left _ -> Null }"#),
        json!("hi")
    );
    assert_eq!(
        run(r#"case (eitherDecode "null" :: Either Text Value) of { Right v -> v; Left _ -> Null }"#),
        json!(null)
    );
}

#[test]
#[ignore = "needs worktree extract binary (TIDEPOOL_EXTRACT) with JsonDecode interception"]
fn decode_array_and_object() {
    assert_eq!(
        run(r#"case (eitherDecode "[1,2,3]" :: Either Text Value) of { Right v -> v; Left _ -> Null }"#),
        json!([1, 2, 3])
    );
    assert_eq!(
        run(r#"case (eitherDecode "{\"a\":1,\"b\":[true,null]}" :: Either Text Value) of { Right v -> v; Left _ -> Null }"#),
        json!({"a": 1, "b": [true, null]})
    );
}

#[test]
#[ignore = "needs worktree extract binary (TIDEPOOL_EXTRACT) with JsonDecode interception"]
fn either_decode_preserves_error() {
    // Right on valid input.
    assert_eq!(
        run(r#"case (eitherDecode "42" :: Either Text Value) of { Right v -> v; Left _ -> Null }"#),
        json!(42)
    );
    // Left branch fires on malformed input...
    assert_eq!(
        run(
            r#"case (eitherDecode "{oops" :: Either Text Value) of { Right _ -> String "right"; Left _ -> String "left" }"#
        ),
        json!("left")
    );
    // ...and the serde error message is preserved (non-empty), never discarded —
    // the whole reason eitherDecode exists over a `Maybe`-shaped decoder.
    assert_eq!(
        run(r#"case (eitherDecode "{oops" :: Either Text Value) of { Left e -> Bool (T.length e > 0); Right _ -> Bool False }"#),
        json!(true)
    );
}

#[test]
#[ignore = "needs worktree extract binary (TIDEPOOL_EXTRACT) with JsonDecode interception"]
fn decode_in_pure_fold() {
    // The headline use case: decode each JSONL line inside a pure fold, no effect.
    assert_eq!(
        run(
            r#"map (\l -> case (eitherDecode l :: Either Text Value) of { Right v -> v; Left _ -> Null }) (T.lines "1\n2\n3")"#
        ),
        json!([1, 2, 3])
    );
}
