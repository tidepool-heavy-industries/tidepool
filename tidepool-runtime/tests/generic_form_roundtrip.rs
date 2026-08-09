//! The operator-form interpreter (`Tidepool.Form.GForm`) executed on the JIT
//! through the real extract pipeline: a `FormShape` derived from a type's
//! `Generic` representation with no value of that type, and a `FormAnswer`
//! decoded back into the typed value.
//!
//! A host-side typecheck would prove nothing here. Generic-representation
//! dictionary elaboration is exactly the part that can typecheck under plain
//! GHC and fail to elaborate under our JIT, so every case below compiles
//! through `tidepool-extract` and runs on the JIT machine.
//!
//! Each test covers a whole coverage class in one compile. The shape tests
//! return the derived shapes and assert them against exact expected renderings;
//! the decode tests return the list of cases that FAILED and assert it is
//! empty, so a failure names the case rather than just the class.
//!
//! Requires a worktree extract binary (`cabal build tidepool-extract-bin`,
//! then `TIDEPOOL_EXTRACT` pointed at it, or run inside `nix develop`). Skips
//! cleanly when the extractor is unreachable.

use serde_json::json;
use tidepool_testing::eval_harness::EvalHarness;

const HEADER: &str = "{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DeriveGeneric, TypeApplications #-}\n\
     module Expr where\n\
     import Tidepool.Prelude hiding (error)\n\
     import Tidepool.Form.Shape\n\
     import Tidepool.Form.GForm\n";

/// Every shape the coverage tests derive from, declared once.
///
/// Nothing here writes an instance of anything in `Tidepool.Form.GForm`:
/// `deriving (Generic)` is the whole author contract, and `Eq`/`Show` are the
/// ordinary domain classes these tests themselves use.
const DECLS: &str = r#"
data Prims = Prims
  { pText :: Text
  , pInt  :: Int
  , pNum  :: Double
  , pFlag :: Bool
  } deriving (Generic, Eq, Show)

data Pos = Pos Text Int deriving (Generic, Eq, Show)

newtype Tag = Tag Text deriving (Generic, Eq, Show)
newtype Boxed = Boxed { unwrap :: Int } deriving (Generic, Eq, Show)

data Pair = Pair { pair :: (Text, Int) } deriving (Generic, Eq, Show)

data Nullary = Nullary deriving (Generic, Eq, Show)
data Unit1 = Unit1 { u :: () } deriving (Generic, Eq, Show)

data Two = TA | TB deriving (Generic, Eq, Show)
data Three = T1 | T2 | T3 deriving (Generic, Eq, Show)
data Five = F1 | F2 | F3 | F4 | F5 deriving (Generic, Eq, Show)

data Dest = LocalHost | Ssh { host :: Text, port :: Int } | Raw Text Int
  deriving (Generic, Eq, Show)

data Scope = CurrentFile | Workspace deriving (Generic, Eq, Show)
data Speed = Fast | Thorough deriving (Generic, Eq, Show)
data Operation = Search Scope | Analyze Speed deriving (Generic, Eq, Show)

data Nested2 = Nested2 { a1 :: Pos, a2 :: Pos } deriving (Generic, Eq, Show)

data Opts = Opts
  { maybeLeaf :: Maybe Text
  , maybeProd :: Maybe Pos
  , maybeSum  :: Maybe Three
  } deriving (Generic, Eq, Show)

data Twin = TwinL Text | TwinR Text deriving (Generic, Eq, Show)

check :: Text -> Bool -> [Text]
check nm ok = if ok then [] else [nm]
"#;

/// Compile + run a module PURE on the JIT and return `result` as JSON. Skips
/// (returns `None`) when the extractor is unavailable.
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
/// `Prims`, a `Dest`, or an `Operation` — the derivation never had a value to
/// look at, which is the property `askUser @T` needs before the operator has
/// answered anything.
///
/// Each shape is asserted through its rendering. That pins every key, every
/// constructor, and the order of both, which is the whole contract — and it
/// reads as the shape an operator's renderer receives.
fn shapes(targets: &[&str]) -> Option<serde_json::Value> {
    let items = targets
        .iter()
        .map(|t| format!("show (formShape @{t})"))
        .collect::<Vec<_>>()
        .join("\n  , ");
    eval_result(&format!("result :: [Text]\nresult =\n  [ {items}\n  ]\n"))
}

/// Every primitive leaf, a named record, a positional product, and a
/// one-field newtype — which stays a named structural boundary rather than
/// collapsing into the type it wraps.
#[test]
fn leaf_product_and_newtype_shapes() {
    if let Some(v) = shapes(&["Prims", "Pos", "Tag", "Boxed"]) {
        assert_eq!(
            v,
            json!([
                r#"ProductShape "Prims" "Prims" [FieldShape "pText" StringShape,FieldShape "pInt" IntShape,FieldShape "pNum" NumberShape,FieldShape "pFlag" BoolShape]"#,
                r#"ProductShape "Pos" "Pos" [FieldShape "1" StringShape,FieldShape "2" IntShape]"#,
                r#"ProductShape "Tag" "Tag" [FieldShape "1" StringShape]"#,
                r#"ProductShape "Boxed" "Boxed" [FieldShape "unwrap" IntShape]"#,
            ])
        )
    }
}

/// A constructor with no fields contributes no control, `()` is a unit leaf,
/// and a tuple is an ordinary positional product whose field keys are its
/// one-based positions.
#[test]
fn unit_nullary_and_tuple_shapes() {
    if let Some(v) = shapes(&["Nullary", "Unit1", "Pair"]) {
        assert_eq!(
            v,
            json!([
                "UnitShape",
                r#"ProductShape "Unit1" "Unit1" [FieldShape "u" UnitShape]"#,
                r#"ProductShape "Pair" "Pair" [FieldShape "pair" (ProductShape "Tuple2" "(,)" [FieldShape "1" StringShape,FieldShape "2" IntShape])]"#,
            ])
        )
    }
}

/// Constructor DECLARATION order is option order, at 2, 3, and 5 constructors.
/// `GHC.Generics` builds a balanced `:+:` tree; none of that shape reaches the
/// variant list.
#[test]
fn sum_shapes_preserve_declaration_order() {
    if let Some(v) = shapes(&["Two", "Three", "Five"]) {
        assert_eq!(
            v,
            json!([
                r#"SumShape "Two" [VariantShape "TA" UnitShape,VariantShape "TB" UnitShape]"#,
                r#"SumShape "Three" [VariantShape "T1" UnitShape,VariantShape "T2" UnitShape,VariantShape "T3" UnitShape]"#,
                r#"SumShape "Five" [VariantShape "F1" UnitShape,VariantShape "F2" UnitShape,VariantShape "F3" UnitShape,VariantShape "F4" UnitShape,VariantShape "F5" UnitShape]"#,
            ])
        )
    }
}

/// A payload-bearing sum: the nullary branch is `UnitShape`, the record branch
/// keys by selector name, and the positional branch keys by position.
///
/// Sum-of-sum is here too: the outer constructors key the choice, and the
/// selected branch contributes its own nested choice.
#[test]
fn payload_bearing_and_nested_sum_shapes() {
    if let Some(v) = shapes(&["Dest", "Operation"]) {
        assert_eq!(
            v,
            json!([
                r#"SumShape "Dest" [VariantShape "LocalHost" UnitShape,VariantShape "Ssh" (ProductShape "Dest" "Ssh" [FieldShape "host" StringShape,FieldShape "port" IntShape]),VariantShape "Raw" (ProductShape "Dest" "Raw" [FieldShape "1" StringShape,FieldShape "2" IntShape])]"#,
                r#"SumShape "Operation" [VariantShape "Search" (ProductShape "Operation" "Search" [FieldShape "1" (SumShape "Scope" [VariantShape "CurrentFile" UnitShape,VariantShape "Workspace" UnitShape])]),VariantShape "Analyze" (ProductShape "Operation" "Analyze" [FieldShape "1" (SumShape "Speed" [VariantShape "Fast" UnitShape,VariantShape "Thorough" UnitShape])])]"#,
            ])
        )
    }
}

/// `Maybe` is an optional control around a leaf, a product, and a sum alike —
/// never a `Nothing`/`Just` constructor picker.
///
/// Positional keys are scoped to their OWN product node: two `Pos` fields in
/// one record both start at "1". There is no form-wide field counter.
#[test]
fn optional_and_node_scoped_positional_shapes() {
    if let Some(v) = shapes(&["Opts", "Nested2"]) {
        assert_eq!(
            v,
            json!([
                r#"ProductShape "Opts" "Opts" [FieldShape "maybeLeaf" (OptionalShape StringShape),FieldShape "maybeProd" (OptionalShape (ProductShape "Pos" "Pos" [FieldShape "1" StringShape,FieldShape "2" IntShape])),FieldShape "maybeSum" (OptionalShape (SumShape "Three" [VariantShape "T1" UnitShape,VariantShape "T2" UnitShape,VariantShape "T3" UnitShape]))]"#,
                r#"ProductShape "Nested2" "Nested2" [FieldShape "a1" (ProductShape "Pos" "Pos" [FieldShape "1" StringShape,FieldShape "2" IntShape]),FieldShape "a2" (ProductShape "Pos" "Pos" [FieldShape "1" StringShape,FieldShape "2" IntShape])]"#,
            ])
        )
    }
}

/// Every structural-coverage case decodes back to the typed value.
///
/// The distinguishability cases are here on purpose: `False`, zero, an empty
/// `Text`, a nullary payload, and the same leaf value under two different
/// constructors all have to survive as themselves rather than collapsing into
/// "absent" or into each other.
#[test]
fn answers_decode_to_typed_values() {
    let body = r#"
result :: [Text]
result = concat
  [ check "prims" (decodeForm @Prims (ProductAnswer
      [ ("pText", StringAnswer "hi"), ("pInt", IntAnswer 3)
      , ("pNum", NumberAnswer 1.5), ("pFlag", BoolAnswer True) ])
      == Right (Prims "hi" 3 1.5 True))
  , check "prims-falsy-values-survive" (decodeForm @Prims (ProductAnswer
      [ ("pText", StringAnswer ""), ("pInt", IntAnswer 0)
      , ("pNum", NumberAnswer 0.0), ("pFlag", BoolAnswer False) ])
      == Right (Prims "" 0 0.0 False))
  , check "positional" (decodeForm @Pos
      (ProductAnswer [("1", StringAnswer "x"), ("2", IntAnswer 7)]) == Right (Pos "x" 7))
  , check "newtype-positional" (decodeForm @Tag
      (ProductAnswer [("1", StringAnswer "t")]) == Right (Tag "t"))
  , check "newtype-record" (decodeForm @Boxed
      (ProductAnswer [("unwrap", IntAnswer 9)]) == Right (Boxed 9))
  , check "tuple" (decodeForm @Pair (ProductAnswer
      [("pair", ProductAnswer [("1", StringAnswer "a"), ("2", IntAnswer 1)])])
      == Right (Pair ("a", 1)))
  , check "unit-field" (decodeForm @Unit1
      (ProductAnswer [("u", UnitAnswer)]) == Right (Unit1 ()))
  , check "nullary-datatype" (decodeForm @Nullary UnitAnswer == Right Nullary)
  , check "sum-of-2-all-constructors" (and
      [ decodeForm @Two (SumAnswer "TA" UnitAnswer) == Right TA
      , decodeForm @Two (SumAnswer "TB" UnitAnswer) == Right TB ])
  , check "sum-of-3-all-constructors" (and
      [ decodeForm @Three (SumAnswer "T1" UnitAnswer) == Right T1
      , decodeForm @Three (SumAnswer "T2" UnitAnswer) == Right T2
      , decodeForm @Three (SumAnswer "T3" UnitAnswer) == Right T3 ])
  , check "sum-of-5-all-constructors" (and
      [ decodeForm @Five (SumAnswer "F1" UnitAnswer) == Right F1
      , decodeForm @Five (SumAnswer "F2" UnitAnswer) == Right F2
      , decodeForm @Five (SumAnswer "F3" UnitAnswer) == Right F3
      , decodeForm @Five (SumAnswer "F4" UnitAnswer) == Right F4
      , decodeForm @Five (SumAnswer "F5" UnitAnswer) == Right F5 ])
  , check "sum-nullary-branch" (decodeForm @Dest
      (SumAnswer "LocalHost" UnitAnswer) == Right LocalHost)
  , check "sum-record-branch" (decodeForm @Dest (SumAnswer "Ssh"
      (ProductAnswer [("host", StringAnswer "example.com"), ("port", IntAnswer 22)]))
      == Right (Ssh "example.com" 22))
  , check "sum-positional-branch" (decodeForm @Dest (SumAnswer "Raw"
      (ProductAnswer [("1", StringAnswer "r"), ("2", IntAnswer 1)]))
      == Right (Raw "r" 1))
  , check "sum-of-sum" (and
      [ decodeForm @Operation (SumAnswer "Search"
          (ProductAnswer [("1", SumAnswer "Workspace" UnitAnswer)]))
          == Right (Search Workspace)
      , decodeForm @Operation (SumAnswer "Analyze"
          (ProductAnswer [("1", SumAnswer "Fast" UnitAnswer)]))
          == Right (Analyze Fast) ])
  , check "nested-positional-nodes-restart-at-1" (decodeForm @Nested2 (ProductAnswer
      [ ("a1", ProductAnswer [("1", StringAnswer "l"), ("2", IntAnswer 1)])
      , ("a2", ProductAnswer [("1", StringAnswer "r"), ("2", IntAnswer 2)]) ])
      == Right (Nested2 (Pos "l" 1) (Pos "r" 2)))
  , check "maybe-absent" (decodeForm @Opts (ProductAnswer
      [ ("maybeLeaf", OptionalAnswer Nothing)
      , ("maybeProd", OptionalAnswer Nothing)
      , ("maybeSum", OptionalAnswer Nothing) ])
      == Right (Opts Nothing Nothing Nothing))
  , check "maybe-present-around-leaf-product-sum" (decodeForm @Opts (ProductAnswer
      [ ("maybeLeaf", OptionalAnswer (Just (StringAnswer "n")))
      , ("maybeProd", OptionalAnswer (Just
          (ProductAnswer [("1", StringAnswer "p"), ("2", IntAnswer 1)])))
      , ("maybeSum", OptionalAnswer (Just (SumAnswer "T2" UnitAnswer))) ])
      == Right (Opts (Just "n") (Just (Pos "p" 1)) (Just T2)))
  , check "empty-text-is-not-absent" (decodeForm @Opts (ProductAnswer
      [ ("maybeLeaf", OptionalAnswer (Just (StringAnswer "")))
      , ("maybeProd", OptionalAnswer Nothing)
      , ("maybeSum", OptionalAnswer Nothing) ])
      == Right (Opts (Just "") Nothing Nothing))
  , check "identical-leaf-in-different-branches" (and
      [ decodeForm @Twin (SumAnswer "TwinL" (ProductAnswer [("1", StringAnswer "same")]))
          == Right (TwinL "same")
      , decodeForm @Twin (SumAnswer "TwinR" (ProductAnswer [("1", StringAnswer "same")]))
          == Right (TwinR "same") ])
  ]
"#;
    if let Some(v) = eval_result(body) {
        assert_eq!(v, json!([]), "cases that did not decode to the typed value")
    }
}

/// Malformed submissions come back as `FormError` VALUES. Nothing throws, so
/// `askUser` can re-present the same form; a bad submission never consumes the
/// continuation.
#[test]
fn malformed_answers_are_rejected_as_data() {
    let body = r#"
result :: [Text]
result = concat
  [ check "missing-field" (decodeForm @Prims (ProductAnswer
      [("pText", StringAnswer "x"), ("pNum", NumberAnswer 1.0), ("pFlag", BoolAnswer True)])
      == (Left (MissingField "pInt") :: Either FormError Prims))
  , check "extra-field" (decodeForm @Prims (ProductAnswer
      [ ("pText", StringAnswer "x"), ("pInt", IntAnswer 1)
      , ("pNum", NumberAnswer 1.0), ("pFlag", BoolAnswer True)
      , ("nope", StringAnswer "?") ])
      == (Left (UnexpectedField "nope") :: Either FormError Prims))
  , check "duplicate-field" (decodeForm @Prims (ProductAnswer
      [ ("pText", StringAnswer "x"), ("pText", StringAnswer "y")
      , ("pInt", IntAnswer 1), ("pNum", NumberAnswer 1.0), ("pFlag", BoolAnswer True) ])
      == (Left (DuplicateField "pText") :: Either FormError Prims))
  , check "unknown-constructor" (decodeForm @Dest (SumAnswer "Nope" UnitAnswer)
      == (Left (UnknownConstructor "Dest" "Nope" ["LocalHost", "Ssh", "Raw"])
          :: Either FormError Dest))
  , check "wrong-leaf-shape" (decodeForm @Prims (ProductAnswer
      [ ("pText", IntAnswer 1), ("pInt", IntAnswer 1)
      , ("pNum", NumberAnswer 1.0), ("pFlag", BoolAnswer True) ])
      == (Left (InField "pText" (ShapeMismatch "text" "a whole number"))
          :: Either FormError Prims))
  , check "not-a-product" (decodeForm @Prims (StringAnswer "x")
      == (Left (ShapeMismatch "a group of fields" "text") :: Either FormError Prims))
  , check "not-a-choice" (decodeForm @Dest (ProductAnswer [])
      == (Left (ShapeMismatch "a choice" "a group of fields") :: Either FormError Dest))
  , check "nullary-branch-rejects-a-payload" (decodeForm @Dest
      (SumAnswer "LocalHost" (ProductAnswer []))
      == (Left (InVariant "LocalHost" (ShapeMismatch "no payload" "a group of fields"))
          :: Either FormError Dest))
  , check "optional-rejects-a-bare-value" (decodeForm @Opts (ProductAnswer
      [ ("maybeLeaf", StringAnswer "n")
      , ("maybeProd", OptionalAnswer Nothing)
      , ("maybeSum", OptionalAnswer Nothing) ])
      == (Left (InField "maybeLeaf" (ShapeMismatch "an optional value" "text"))
          :: Either FormError Opts))
  , check "nested-error-keeps-its-path" (decodeForm @Opts (ProductAnswer
      [ ("maybeLeaf", OptionalAnswer Nothing)
      , ("maybeProd", OptionalAnswer (Just
          (ProductAnswer [("1", IntAnswer 1), ("2", IntAnswer 2)])))
      , ("maybeSum", OptionalAnswer Nothing) ])
      == (Left (InField "maybeProd" (InField "1" (ShapeMismatch "text" "a whole number")))
          :: Either FormError Opts))
  ]
"#;
    if let Some(v) = eval_result(body) {
        assert_eq!(v, json!([]), "cases that were not rejected as expected")
    }
}

/// An unknown constructor tag is reported ONCE, against the sum's own variant
/// list — not accumulated per `:+:` branch.
///
/// The spike's left-to-right decode produced `no constructor Nope | no
/// constructor Nope | no constructor Nope`: one useless line per branch, and
/// no statement of what the operator could have picked instead.
#[test]
fn unknown_constructor_names_the_valid_choices_once() {
    tidepool_testing::eval_harness::require_extract();
    let body = r#"
result :: Text
result = case decodeForm @Dest (SumAnswer "Nope" UnitAnswer) of
  Left e -> renderFormError e
  Right _ -> "decoded an answer it should have rejected"
"#;
    let src = format!("{HEADER}{DECLS}\n{body}");
    let v = EvalHarness::new()
        .with_stdlib()
        .run_pure(&src, "result")
        .expect("compile_and_run_pure failed")
        .to_json();
    assert_eq!(
        v,
        json!("unknown constructor Nope for Dest; expected one of LocalHost, Ssh, Raw")
    );
}

/// A DERIVED sum shape compared against its hand-written literal.
///
/// This exact comparison case-trapped while the interpreter was being built —
/// a tag-as-address escape, isolated to derived-SUM vs sum-literal (derived
/// product vs product literal, literal vs literal, and `show` of a derived sum
/// were all fine). The shape tests above assert exact RENDERINGS instead,
/// which pins every key, constructor and order without needing `==` over a
/// derived sum, so coverage never depended on this.
///
/// It is asserted here because the `==` form is the natural thing to write and
/// should either work or fail loudly. After `strlen-hardening`, a regression
/// surfaces as a `ShapeTrapKind::AddrKind` poison+breadcrumb trap naming the
/// bad unbox rather than a `runtime_strlen` segfault.
#[test]
fn derived_sum_shape_equals_its_literal() {
    if let Some(v) = eval_result(
        r#"result :: [Text]
result =
  check "Two" (formShape @Two == SumShape "Two" [VariantShape "TA" UnitShape, VariantShape "TB" UnitShape])
"#,
    ) {
        assert_eq!(v, json!([]), "a derived sum shape did not equal its literal")
    }
}
