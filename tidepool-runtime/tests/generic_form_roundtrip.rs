//! `askUser @T`'s structural coverage: shapes derive from type metadata
//! alone (no value of `T` exists yet), and the submitted PLAIN JSON decodes
//! back through `T`'s own generic `FromJSON` — there is no parallel answer
//! language. Positional payload fields are rejected at COMPILE time (they
//! have no selector to key a control or a JSON field by).
//!
//! Needs `TIDEPOOL_EXTRACT` (run inside `nix develop`).

use serde_json::json;
use tidepool_testing::eval_harness::EvalHarness;

const HEADER: &str =
    "{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DeriveGeneric, DeriveAnyClass, TypeApplications #-}\n\
     module Expr where\n\
     import Tidepool.Prelude hiding (error)\n\
     import Tidepool.Form.Shape\n\
     import Tidepool.Form.GForm\n";

/// Every shape the coverage tests derive from, declared once. All payload
/// fields are NAMED (record syntax) — the author contract the interpreter
/// now enforces at compile time.
///
/// `deriving (Generic, FromJSON)` is the whole author contract, and
/// `Eq`/`Show` are the ordinary domain classes these tests themselves use.
const DECLS: &str = r#"
data Prims = Prims
  { pText :: Text
  , pInt  :: Int
  , pNum  :: Double
  , pFlag :: Bool
  } deriving (Generic, Eq, Show, FromJSON)

newtype Boxed = Boxed { unwrap :: Int } deriving (Generic, Eq, Show, FromJSON)

data Nullary = Nullary deriving (Generic, Eq, Show, FromJSON)
data Unit1 = Unit1 { u :: () } deriving (Generic, Eq, Show, FromJSON)

data Two = TA | TB deriving (Generic, Eq, Show, FromJSON)
data Three = T1 | T2 | T3 deriving (Generic, Eq, Show, FromJSON)
data Five = F1 | F2 | F3 | F4 | F5 deriving (Generic, Eq, Show, FromJSON)

data Dest = LocalHost | Ssh { host :: Text, port :: Int }
  deriving (Generic, Eq, Show, FromJSON)

data Named = Named { inner :: Three } deriving (Generic, Eq, Show, FromJSON)

data Nested2 = Nested2 { a1 :: Boxed, a2 :: Boxed } deriving (Generic, Eq, Show, FromJSON)

data Opts = Opts
  { maybeLeaf :: Maybe Text
  , maybeSum  :: Maybe Three
  } deriving (Generic, Eq, Show, FromJSON)

data Twin = TwinL { tl :: Text } | TwinR { tr :: Text }
  deriving (Generic, Eq, Show, FromJSON)

check :: Text -> Bool -> [Text]
check nm ok = if ok then [] else [nm]
"#;

/// Compile + run a module PURE on the JIT and return `result` as JSON.
fn eval_result(body: &str) -> Option<serde_json::Value> {
    tidepool_testing::eval_harness::require_extract();
    let src = format!("{HEADER}{DECLS}\n{body}");
    Some(
        EvalHarness::new()
            .with_stdlib()
            .run_pure(&src, "result")
            .expect("compile_and_run_pure failed")
            .to_json(),
    )
}

/// Shapes come from type metadata alone. Nothing in this module constructs a
/// `Prims` or a `Dest` — the derivation never had a value to look at, which
/// is the property `askUser @T` needs before the operator has answered
/// anything.
fn shapes(targets: &[&str]) -> Option<serde_json::Value> {
    let items = targets
        .iter()
        .map(|t| format!("show (formShape @{t})"))
        .collect::<Vec<_>>()
        .join("\n  , ");
    eval_result(&format!("result :: [Text]\nresult =\n  [ {items}\n  ]\n"))
}

/// Every primitive leaf, a named record, and a one-field record newtype —
/// which stays a named structural boundary rather than collapsing into the
/// type it wraps.
#[test]
fn leaf_product_and_newtype_shapes() {
    if let Some(v) = shapes(&["Prims", "Boxed"]) {
        assert_eq!(
            v,
            json!([
                r#"ProductShape "Prims" "Prims" [FieldShape "pText" StringShape,FieldShape "pInt" IntShape,FieldShape "pNum" NumberShape,FieldShape "pFlag" BoolShape]"#,
                r#"ProductShape "Boxed" "Boxed" [FieldShape "unwrap" IntShape]"#,
            ])
        )
    }
}

#[test]
fn unit_and_nullary_shapes() {
    if let Some(v) = shapes(&["Nullary", "Unit1"]) {
        assert_eq!(
            v,
            json!([
                r#"ProductShape "Nullary" "Nullary" []"#,
                r#"ProductShape "Unit1" "Unit1" [FieldShape "u" UnitShape]"#,
            ])
        )
    }
}

/// Sum variants surface in DECLARATION order, not the balanced `:+:` tree's.
#[test]
fn sum_shapes_preserve_declaration_order() {
    if let Some(v) = shapes(&["Two", "Three", "Five"]) {
        assert_eq!(
            v,
            json!([
                r#"SumShape "Two" [VariantShape "TA" (ProductShape "Two" "TA" []),VariantShape "TB" (ProductShape "Two" "TB" [])]"#,
                r#"SumShape "Three" [VariantShape "T1" (ProductShape "Three" "T1" []),VariantShape "T2" (ProductShape "Three" "T2" []),VariantShape "T3" (ProductShape "Three" "T3" [])]"#,
                r#"SumShape "Five" [VariantShape "F1" (ProductShape "Five" "F1" []),VariantShape "F2" (ProductShape "Five" "F2" []),VariantShape "F3" (ProductShape "Five" "F3" []),VariantShape "F4" (ProductShape "Five" "F4" []),VariantShape "F5" (ProductShape "Five" "F5" [])]"#,
            ])
        )
    }
}

#[test]
fn payload_bearing_and_nested_shapes() {
    if let Some(v) = shapes(&["Dest", "Named", "Nested2", "Opts"]) {
        assert_eq!(
            v,
            json!([
                r#"SumShape "Dest" [VariantShape "LocalHost" (ProductShape "Dest" "LocalHost" []),VariantShape "Ssh" (ProductShape "Dest" "Ssh" [FieldShape "host" StringShape,FieldShape "port" IntShape])]"#,
                r#"ProductShape "Named" "Named" [FieldShape "inner" (SumShape "Three" [VariantShape "T1" (ProductShape "Three" "T1" []),VariantShape "T2" (ProductShape "Three" "T2" []),VariantShape "T3" (ProductShape "Three" "T3" [])])]"#,
                r#"ProductShape "Nested2" "Nested2" [FieldShape "a1" (ProductShape "Boxed" "Boxed" [FieldShape "unwrap" IntShape]),FieldShape "a2" (ProductShape "Boxed" "Boxed" [FieldShape "unwrap" IntShape])]"#,
                r#"ProductShape "Opts" "Opts" [FieldShape "maybeLeaf" (OptionalShape StringShape),FieldShape "maybeSum" (OptionalShape (SumShape "Three" [VariantShape "T1" (ProductShape "Three" "T1" []),VariantShape "T2" (ProductShape "Three" "T2" []),VariantShape "T3" (ProductShape "Three" "T3" [])]))]"#,
            ])
        )
    }
}

/// A POSITIONAL payload field is a compile-time TypeError naming the fix —
/// the successor to the deleted numeric-position keys.
#[test]
fn positional_field_is_rejected_at_compile_time() {
    tidepool_testing::eval_harness::require_extract();
    let src = format!(
        "{HEADER}{DECLS}\ndata P = P Text Int deriving (Generic)\n\nresult :: Text\nresult = show (formShape @P)\n"
    );
    match EvalHarness::new().with_stdlib().compile(&src, "result") {
        Ok(_) => panic!("a positional payload field must not derive a form"),
        Err(e) => {
            let msg = tidepool_runtime::classify_compile(&e).message;
            assert!(
                msg.contains("record syntax"),
                "expected the positional-field TypeError, got:\n{msg}"
            );
        }
    }
}

/// Distinguishability under the ONE generic decode: `False`, zero, an empty
/// `Text`, an absent optional, and the same leaf under two different
/// constructors all survive as themselves.
#[test]
fn plain_json_answers_decode_to_typed_values() {
    let body = r#"
result :: [Text]
result = concat
  [ check "prims" (fromJSON (object
      [ ("pText", String "hi"), ("pInt", toJSON (3 :: Int))
      , ("pNum", toJSON (1.5 :: Double)), ("pFlag", Bool True) ])
      == Success (Prims "hi" 3 1.5 True))
  , check "empty-record" (fromJSON (object []) == Success Nullary)
  , check "unit-null" (fromJSON Null == Success ())
  , check "prims-falsy-values-survive" (fromJSON (object
      [ ("pText", String ""), ("pInt", toJSON (0 :: Int))
      , ("pNum", toJSON (0.0 :: Double)), ("pFlag", Bool False) ])
      == Success (Prims "" 0 0.0 False))
  , check "enums" (and
      [ fromJSON (String "TA") == Success TA
      , fromJSON (String "T3") == Success T3
      , fromJSON (String "F5") == Success F5 ])
  , check "mixed-sum-both-branches" (and
      [ fromJSON (object [("tag", String "LocalHost")]) == Success LocalHost
      , fromJSON (object [("tag", String "Ssh"), ("host", String "h"), ("port", toJSON (2 :: Int))])
          == Success (Ssh "h" 2) ])
  , check "nested-enum-field" (fromJSON (object [("inner", String "T2")])
      == Success (Named T2))
  , check "maybe-absent-and-null" (and
      [ fromJSON (object []) == Success (Opts Nothing Nothing)
      , fromJSON (object [("maybeLeaf", Null), ("maybeSum", Null)])
          == Success (Opts Nothing Nothing) ])
  , check "maybe-present" (fromJSON (object
      [("maybeLeaf", String "n"), ("maybeSum", String "T2")])
      == Success (Opts (Just "n") (Just T2)))
  , check "empty-text-is-not-absent" (fromJSON (object [("maybeLeaf", String "")])
      == Success (Opts (Just "") Nothing))
  , check "identical-leaf-in-different-branches" (and
      [ fromJSON (object [("tag", String "TwinL"), ("tl", String "same")]) == Success (TwinL "same")
      , fromJSON (object [("tag", String "TwinR"), ("tr", String "same")]) == Success (TwinR "same") ])
  , check "missing-field-rejected"
      (case fromJSON (object [("pText", String "x")]) :: Result Prims of
         Error _ -> True
         Success _ -> False)
  , check "wrong-kind-rejected"
      (case fromJSON (object
        [ ("pText", String "x"), ("pInt", String "not a number")
        , ("pNum", toJSON (1.0 :: Double)), ("pFlag", Bool True) ]) :: Result Prims of
         Error _ -> True
         Success _ -> False)
  ]
"#;
    if let Some(v) = eval_result(body) {
        assert_eq!(v, json!([]), "cases that did not decode to the typed value")
    }
}
