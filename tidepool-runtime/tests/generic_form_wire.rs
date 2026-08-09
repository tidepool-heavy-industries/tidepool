//! `Tidepool.Form.Wire` against the JSON contract `tidepool-harness`'s
//! `selfharness::operator` module docs define — the encoder/decoder that puts
//! a `FormShape` on the operator wire and reads a `FormAnswer` back off it.
//!
//! The Rust side OWNS that encoding (it is what `#[derive(Serialize)]` with
//! `rename_all = "snake_case"` produces, and `operator.rs`'s own tests assert
//! it there). This file asserts the Haskell half against the SAME worked
//! examples, quoted verbatim from those module docs rather than paraphrased —
//! a paraphrase of a wire contract is not the contract, and a mismatch here
//! is the whole risk of the generic-form path.
//!
//! Each expected JSON literal below is parsed to a `Value` and compared
//! structurally, so object-key ORDER (a `Map` in Haskell, a struct in Rust)
//! is not part of what is being asserted — only the shape, keys, and values
//! are.
//!
//! Runs through the real extract/JIT for the same reason
//! `generic_form_roundtrip.rs` does: generic-representation dictionary
//! elaboration is the part that can typecheck under plain GHC and fail to
//! elaborate under our JIT. Skips cleanly when the extractor is unreachable.

use serde_json::json;
use tidepool_testing::eval_harness::EvalHarness;

const HEADER: &str = "{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DeriveGeneric, TypeApplications #-}\n\
     module Expr where\n\
     import Tidepool.Prelude hiding (error)\n\
     import Tidepool.Aeson.Value (eitherDecodeValue)\n\
     import Tidepool.Form.Shape\n\
     import Tidepool.Form.GForm\n\
     import Tidepool.Form.Wire\n";

/// The PRD's own example types (`14-generic-derived-askuser-prd.md`), which
/// are also the fixture `operator.rs`'s worked examples are written against.
///
/// `deriving (Generic)` plus the ordinary domain classes these tests
/// themselves use. No codec, no form builder, no instance of anything in
/// `Tidepool.Form.*`.
const DECLS: &str = r#"
data Environment = Development | Staging | Production
  deriving (Generic, Eq, Show)

data Destination
  = LocalHost
  | Ssh { host :: Text, port :: Int }
  | Container { image :: Text }
  deriving (Generic, Eq, Show)

data DeployRequest = DeployRequest
  { service       :: Text
  , environment   :: Environment
  , destination   :: Destination
  , replicas      :: Int
  , runMigrations :: Bool
  , releaseNote   :: Maybe Text
  } deriving (Generic, Eq, Show)

-- Parse a documented JSON literal. A literal that does not parse yields a
-- marker value that cannot equal any encoding, so a broken literal fails the
-- case it appears in rather than passing vacuously.
wire :: Text -> Value
wire t = case eitherDecodeValue t of
  Right v -> v
  Left _  -> String "<<unparseable json literal>>"

check :: Text -> Bool -> [Text]
check nm ok = if ok then [] else [nm]
"#;

/// Compile + run a module PURE on the JIT and return `result` as JSON. Skips
/// (returns `None`) when the extractor is unavailable.
fn eval_result(body: &str) -> Option<serde_json::Value> {
    if !tidepool_testing::eval_harness::extract_available() {
        eprintln!("skipping: tidepool-extract unavailable (set TIDEPOOL_EXTRACT / nix develop)");
        return None;
    }
    let src = format!("{HEADER}{DECLS}\n{body}");
    Some(
        EvalHarness::new()
            .with_stdlib()
            .run_pure(&src, "result")
            .expect("compile_and_run_pure failed")
            .to_json(),
    )
}

/// Every LEAF and the optional container, each encoded as `operator.rs`
/// documents it — carried inside one product node so the whole set crosses as
/// a single `Value`.
///
/// All the shape assertions in this file are made from RUST, over the encoded
/// `Value` the eval hands back. That is both the crossing production uses
/// (`askUser @T` ships this exact JSON) and a deliberate avoidance: batching
/// many encoded shape literals into one module trips a live codegen defect on
/// this branch — the `runtime_strlen: bad pointer 0x0` / `TypeMetadata`
/// class that `generic_form_roundtrip::derived_sum_shape_equals_its_literal`
/// tracks. It is aggregation-sensitive, not encoding-sensitive: each
/// comparison passes alone, and the production path is green. Keeping each
/// assertion to ONE crossed `Value` sidesteps it without weakening anything —
/// these are still the documented JSON literals, matched structurally.
#[test]
fn leaf_and_optional_shapes_encode_as_documented() {
    let body = r#"
result :: Value
result = encodeShape (ProductShape "Leaves" "Leaves"
  [ FieldShape "text" StringShape
  , FieldShape "whole" IntShape
  , FieldShape "number" NumberShape
  , FieldShape "flag" BoolShape
  , FieldShape "nothing" UnitShape
  , FieldShape "maybeText" (OptionalShape StringShape)
  ])
"#;
    if let Some(v) = eval_result(body) {
        assert_eq!(
            v,
            json!({"product": {
                "type_key": "Leaves",
                "constructor": "Leaves",
                "fields": [
                    {"key": "text", "shape": "string"},
                    {"key": "whole", "shape": "int"},
                    {"key": "number", "shape": "number"},
                    {"key": "flag", "shape": "bool"},
                    {"key": "nothing", "shape": "unit"},
                    {"key": "maybeText", "shape": {"optional": "string"}}
                ]
            }}),
            "a leaf did not encode as documented"
        )
    }
}

/// The docs' record-product worked example, verbatim: a single-constructor
/// `Ssh { host :: Text, port :: Int }`, whose `type_key` and `constructor`
/// are both its own name.
#[test]
fn record_product_shape_encodes_as_documented() {
    let body = r#"
result :: Value
result = encodeShape
  (ProductShape "Ssh" "Ssh" [FieldShape "host" StringShape, FieldShape "port" IntShape])
"#;
    if let Some(v) = eval_result(body) {
        assert_eq!(
            v,
            json!({"product": {
                "type_key": "Ssh",
                "constructor": "Ssh",
                "fields": [
                    {"key": "host", "shape": "string"},
                    {"key": "port", "shape": "int"}
                ]
            }})
        )
    }
}

// The docs' nullary-sum worked example is asserted by
// `derived_shape_crosses_the_wire_as_documented_json` below, whose expected
// JSON contains that exact `Environment` sum encoding — DERIVED, which is
// also how production produces it.
//
// There is deliberately no literal-`SumShape` counterpart here. Encoding a
// literal `SumShape` carrying TWO OR MORE `VariantShape`s dies on this
// branch with `[JIT] runtime_error kind=4 (TypeMetadata)` +
// `runtime_strlen: bad pointer 0x0`, with no `==`, no derived value, and no
// list-typed result involved:
//
//     result :: Value
//     result = encodeShape (SumShape "E" [ VariantShape "A" UnitShape
//                                        , VariantShape "B" UnitShape ])
//
// Measured on this branch (2026-08-09): a ONE-variant literal sum is green,
// a literal product with two and with six fields is green, and the same
// multi-variant sum DERIVED via `formShape @T` is green. That inverts the
// isolation recorded for `generic_form_roundtrip::derived_sum_shape_equals_its_literal`
// ("literal-vs-literal sums are fine") — the sum LITERAL is the trigger, and
// `==` is not required to reach it. Same defect class, escalated with this
// repro; nothing here works around it beyond not building that literal.

/// The two halves tied together: a shape DERIVED from a type — no value of
/// it exists — encoded and handed ACROSS the boundary as JSON, which is
/// exactly what `askUser @T` ships through `AskUserWith` and what the
/// operator gate decodes into `selfharness::operator::FormShape`.
///
/// Asserted from RUST, on the `Value` the eval returns, rather than by
/// comparing against a literal inside Haskell. Two reasons, and both matter:
/// it exercises the real crossing (`value_to_json` over the encoded
/// `Value`), and Haskell `==` between a DERIVED SUM and a literal is a live
/// codegen defect on this branch (`runtime_strlen` bad pointer /
/// `[CASE TRAP]`, tracked by
/// `generic_form_roundtrip::derived_sum_shape_equals_its_literal`) that has
/// nothing to do with the encoding under test here.
#[test]
fn derived_shape_crosses_the_wire_as_documented_json() {
    let Some(v) = eval_result("result :: Value\nresult = encodeShape (formShape @DeployRequest)\n")
    else {
        return;
    };
    assert_eq!(
        v,
        json!({"product": {
            "type_key": "DeployRequest",
            "constructor": "DeployRequest",
            "fields": [
                {"key": "service", "shape": "string"},
                {"key": "environment", "shape": {"sum": {
                    "type_key": "Environment",
                    "variants": [
                        {"constructor": "Development", "shape": "unit"},
                        {"constructor": "Staging", "shape": "unit"},
                        {"constructor": "Production", "shape": "unit"}
                    ]
                }}},
                {"key": "destination", "shape": {"sum": {
                    "type_key": "Destination",
                    "variants": [
                        {"constructor": "LocalHost", "shape": "unit"},
                        {"constructor": "Ssh", "shape": {"product": {
                            "type_key": "Destination",
                            "constructor": "Ssh",
                            "fields": [
                                {"key": "host", "shape": "string"},
                                {"key": "port", "shape": "int"}
                            ]
                        }}},
                        {"constructor": "Container", "shape": {"product": {
                            "type_key": "Destination",
                            "constructor": "Container",
                            "fields": [{"key": "image", "shape": "string"}]
                        }}}
                    ]
                }}},
                {"key": "replicas", "shape": "int"},
                {"key": "runMigrations", "shape": "bool"},
                {"key": "releaseNote", "shape": {"optional": "string"}}
            ]
        }}),
        "the derived DeployRequest form did not reach the wire as documented"
    )
}

/// Every `FormAnswer` worked example, both directions: the encoding matches
/// the documented JSON, and decoding that JSON gives the answer back.
///
/// The four frozen encoding rules are each pinned by a case here — bare
/// product for a single-constructor datatype, `Sum` wrapper even for a
/// nullary branch, `"unit"` (not an empty product) as a nullary payload, and
/// `Optional` rather than a `Nothing`/`Just` pick.
#[test]
fn answers_encode_and_decode_as_the_documented_contract() {
    let body = r#"
result :: [Text]
result = concat
  [ check "leaf-encode" (encodeAnswer (StringAnswer "api") == wire "{\"string\":\"api\"}")
  , check "leaf-decode" (decodeAnswer (wire "{\"string\":\"api\"}") == Just (StringAnswer "api"))
  , check "int-encode" (encodeAnswer (IntAnswer 22) == wire "{\"int\":22}")
  , check "int-decode" (decodeAnswer (wire "{\"int\":22}") == Just (IntAnswer 22))
  , check "number-encode" (encodeAnswer (NumberAnswer 1.5) == wire "{\"number\":1.5}")
  , check "number-decode" (decodeAnswer (wire "{\"number\":1.5}") == Just (NumberAnswer 1.5))
  , check "bool-encode" (encodeAnswer (BoolAnswer False) == wire "{\"bool\":false}")
  , check "bool-decode" (decodeAnswer (wire "{\"bool\":false}") == Just (BoolAnswer False))
  , check "optional-present-encode"
      (encodeAnswer (OptionalAnswer (Just (StringAnswer "hotfix")))
        == wire "{\"optional\":{\"string\":\"hotfix\"}}")
  , check "optional-present-decode"
      (decodeAnswer (wire "{\"optional\":{\"string\":\"hotfix\"}}")
        == Just (OptionalAnswer (Just (StringAnswer "hotfix"))))
  , check "optional-absent-encode"
      (encodeAnswer (OptionalAnswer Nothing) == wire "{\"optional\":null}")
  , check "optional-absent-decode"
      (decodeAnswer (wire "{\"optional\":null}") == Just (OptionalAnswer Nothing))
  , check "record-product-encode" (encodeAnswer sshAnswer
      == wire "{\"product\":[[\"host\",{\"string\":\"example.com\"}],[\"port\",{\"int\":22}]]}")
  , check "record-product-decode"
      (decodeAnswer (wire "{\"product\":[[\"host\",{\"string\":\"example.com\"}],[\"port\",{\"int\":22}]]}")
        == Just sshAnswer)
  , check "nullary-branch-encode"
      (encodeAnswer (SumAnswer "Staging" UnitAnswer)
        == wire "{\"sum\":{\"constructor\":\"Staging\",\"payload\":\"unit\"}}")
  , check "nullary-branch-decode"
      (decodeAnswer (wire "{\"sum\":{\"constructor\":\"Staging\",\"payload\":\"unit\"}}")
        == Just (SumAnswer "Staging" UnitAnswer))
  , check "payload-bearing-branch-encode" (encodeAnswer (SumAnswer "Ssh" sshAnswer)
      == wire "{\"sum\":{\"constructor\":\"Ssh\",\"payload\":{\"product\":[[\"host\",{\"string\":\"example.com\"}],[\"port\",{\"int\":22}]]}}}")
  , check "payload-bearing-branch-decode"
      (decodeAnswer (wire "{\"sum\":{\"constructor\":\"Ssh\",\"payload\":{\"product\":[[\"host\",{\"string\":\"example.com\"}],[\"port\",{\"int\":22}]]}}}")
        == Just (SumAnswer "Ssh" sshAnswer))
  , check "nullary-payload-is-not-an-empty-product"
      (encodeAnswer UnitAnswer /= encodeAnswer (ProductAnswer []))
  , check "nullary-payload-encode" (encodeAnswer UnitAnswer == wire "\"unit\"")
  , check "empty-product-encode" (encodeAnswer (ProductAnswer []) == wire "{\"product\":[]}")
  , check "duplicate-keys-survive-the-wire"
      (decodeAnswer (wire "{\"product\":[[\"a\",{\"int\":1}],[\"a\",{\"int\":2}]]}")
        == Just (ProductAnswer [("a", IntAnswer 1), ("a", IntAnswer 2)]))
  , check "not-an-answer-is-rejected" (decodeAnswer (wire "{\"nope\":1}") == Nothing)
  , check "product-as-object-is-rejected"
      (decodeAnswer (wire "{\"product\":{\"host\":{\"string\":\"x\"}}}") == Nothing)
  , check "fractional-int-is-rejected" (decodeAnswer (wire "{\"int\":1.5}") == Nothing)
  ]
  where
    sshAnswer = ProductAnswer
      [ ("host", StringAnswer "example.com"), ("port", IntAnswer 22) ]
"#;
    if let Some(v) = eval_result(body) {
        assert_eq!(v, json!([]), "answers that did not match the documented contract")
    }
}

/// The nested product-of-sum worked example — `operator.rs`'s largest, the
/// one its own `deploy_request_nested_product_of_sum_round_trips` test
/// asserts on the Rust side — decoded off the wire and rebuilt into the
/// typed `DeployRequest`.
///
/// This is the full crossing in one case: documented JSON → `FormAnswer` →
/// the agent's own ADT, with every selector and constructor key matching
/// verbatim.
#[test]
fn documented_deploy_request_answer_rebuilds_the_typed_value() {
    let body = r#"
result :: [Text]
result = concat
  [ check "decodes-to-a-form-answer" (decodeAnswer (wire submitted) == Just expectedAnswer)
  , check "rebuilds-the-typed-value"
      (fmap (decodeForm @DeployRequest) (decodeAnswer (wire submitted))
        == Just (Right expected))
  , check "round-trips-back-to-the-same-json"
      (fmap encodeAnswer (decodeAnswer (wire submitted)) == Just (wire submitted))
  ]
  where
    submitted = "{\"product\":[[\"service\",{\"string\":\"api\"}],[\"environment\",{\"sum\":{\"constructor\":\"Staging\",\"payload\":\"unit\"}}],[\"destination\",{\"sum\":{\"constructor\":\"Ssh\",\"payload\":{\"product\":[[\"host\",{\"string\":\"example.com\"}],[\"port\",{\"int\":22}]]}}}],[\"replicas\",{\"int\":3}],[\"runMigrations\",{\"bool\":false}],[\"releaseNote\",{\"optional\":{\"string\":\"hotfix\"}}]]}"
    expectedAnswer = ProductAnswer
      [ ("service", StringAnswer "api")
      , ("environment", SumAnswer "Staging" UnitAnswer)
      , ("destination", SumAnswer "Ssh" (ProductAnswer
          [("host", StringAnswer "example.com"), ("port", IntAnswer 22)]))
      , ("replicas", IntAnswer 3)
      , ("runMigrations", BoolAnswer False)
      , ("releaseNote", OptionalAnswer (Just (StringAnswer "hotfix")))
      ]
    expected = DeployRequest
      { service = "api"
      , environment = Staging
      , destination = Ssh { host = "example.com", port = 22 }
      , replicas = 3
      , runMigrations = False
      , releaseNote = Just "hotfix"
      }
"#;
    if let Some(v) = eval_result(body) {
        assert_eq!(
            v,
            json!([]),
            "the documented DeployRequest submission did not cross to the typed value"
        )
    }
}
