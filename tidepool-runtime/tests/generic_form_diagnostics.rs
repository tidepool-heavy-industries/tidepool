//! Compile-fail fixtures for the operator-form interpreter.
//!
//! Compile errors are part of this API. An agent that writes an underivable
//! type must be told which FIELD is wrong and what to write instead, in the
//! vocabulary it used — so each fixture asserts the USEFUL line and lets the
//! surrounding GHC wording vary.
//!
//! The recursive-type fixture is the load-bearing one. `data Tree = Leaf |
//! Node { left :: Tree, right :: Tree }` with a generic form interpreter that
//! merely walks the representation COMPILES: the instances are all found, and
//! the recursion is at the value level, so shape production diverges only when
//! something forces it. Rejection at compile time is a correctness
//! requirement, not a diagnostics-quality nicety, and this is where it is
//! pinned.
//!
//! Requires a worktree extract binary (`cabal build tidepool-extract-bin`,
//! then `TIDEPOOL_EXTRACT` pointed at it, or run inside `nix develop`). Skips
//! cleanly when the extractor is unreachable.

use tidepool_testing::eval_harness::EvalHarness;

const HEADER: &str = "{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DeriveGeneric, TypeApplications #-}\n\
     module Expr where\n\
     import Tidepool.Prelude hiding (error)\n\
     import Tidepool.Form.Shape\n\
     import Tidepool.Form.GForm\n";

/// Compile `decls` + a `formShape @T` use site and return GHC's diagnostic
/// text. Panics if it COMPILES — an underivable type that typechecks is the
/// failure mode these fixtures exist to catch.
fn rejection(decls: &str) -> Option<String> {
    tidepool_testing::eval_harness::require_extract();
    let src = format!("{HEADER}{decls}");
    match EvalHarness::new().with_stdlib().compile(&src, "result") {
        Ok(_) => panic!("expected a compile-time rejection, but this compiled:\n{src}"),
        Err(e) => Some(tidepool_runtime::classify_compile(&e).message),
    }
}

/// Assert every needle appears somewhere in the diagnostic, and that no needle
/// forces the reader through a generic representation type.
fn assert_says(msg: &str, needles: &[&str]) {
    for needle in needles {
        assert!(
            msg.contains(needle),
            "diagnostic should contain {needle:?}, got:\n{msg}"
        );
    }
}

/// A `String` field gets the Text correction, not the generic list advice —
/// `[Char]` is matched before `[a]` for exactly this reason.
#[test]
fn string_field_says_use_text() {
    let decls = r#"
data Q = Q { name :: String } deriving (Generic)

result :: Text
result = show (formShape @Q)
"#;
    if let Some(msg) = rejection(decls) {
        assert_says(&msg, &["`name :: String` is not supported; use Text."]);
    }
}

/// Lists need a repeated-field editor and are out of v1. They are LEGAL in
/// checkpoints and answer synopses — those run their own interpreters — so the
/// message scopes its claim to forms rather than to lists in general.
#[test]
fn list_field_names_the_field() {
    let decls = r#"
data Q = Q { tags :: [Text], title :: Text } deriving (Generic)

result :: Text
result = show (formShape @Q)
"#;
    if let Some(msg) = rejection(decls) {
        assert_says(&msg, &["`tags` is a list", "not supported in v1"]);
    }
}

#[test]
fn map_field_names_the_field() {
    let decls = r#"
data Q = Q { config :: Map Text Text } deriving (Generic)

result :: Text
result = show (formShape @Q)
"#;
    if let Some(msg) = rejection(decls) {
        assert_says(&msg, &["`config` is a Map", "not supported in v1"]);
    }
}

#[test]
fn function_field_names_the_field() {
    let decls = r#"
data Q = Q { callback :: Int -> Int } deriving (Generic)

result :: Text
result = show (formShape @Q)
"#;
    if let Some(msg) = rejection(decls) {
        assert_says(&msg, &["`callback` is a function"]);
    }
}

/// Three states cannot be communicated by one optional control, so nested
/// optionality is rejected with the two ways out.
#[test]
fn nested_maybe_offers_both_corrections() {
    let decls = r#"
data Q = Q { note :: Maybe (Maybe Text) } deriving (Generic)

result :: Text
result = show (formShape @Q)
"#;
    if let Some(msg) = rejection(decls) {
        assert_says(
            &msg,
            &[
                "`note` has nested optionality",
                "Use one Maybe layer",
                "domain-named constructors",
            ],
        );
    }
}

/// The recursion fixture: this must fail to COMPILE, not diverge when forced.
///
/// The message names the field that closes the cycle rather than the type, so
/// an author with a mutually recursive pair is told where to cut.
#[test]
fn recursive_type_is_rejected_at_compile_time() {
    let decls = r#"
data Tree = Leaf | Node { left :: Tree, right :: Tree } deriving (Generic)

result :: Text
result = show (formShape @Tree)
"#;
    if let Some(msg) = rejection(decls) {
        assert_says(
            &msg,
            &[
                "Cannot derive a finite form for recursive field `left`",
                "not supported in v1",
            ],
        );
    }
}

/// Mutual recursion closes the cycle one level down from where it started; the
/// visited set is threaded through every instance, so the field that closes it
/// is still the one named.
#[test]
fn mutually_recursive_types_are_rejected_at_compile_time() {
    let decls = r#"
data Aa = Aa { toB :: Bb } deriving (Generic)
data Bb = Bb { toA :: Aa } deriving (Generic)

result :: Text
result = show (formShape @Aa)
"#;
    if let Some(msg) = rejection(decls) {
        assert_says(&msg, &["Cannot derive a finite form for recursive field"]);
    }
}

/// A field whose type has no `Generic` instance names the FIELD and the
/// TYPE, not just the root the author asked for.
///
/// GHC's own "No instance for (Generic Environment)" is part of the output and
/// is correct; what it cannot say is which field led there. Whether a type has
/// an instance is not something a type family can observe — a `Rep a` that
/// does not reduce is stuck, not apart — so the field name is carried in the
/// unsolved constraint that names it.
#[test]
fn missing_generic_names_the_field_and_the_type() {
    let decls = r#"
data Environment = Development | Staging
data Q = Q { environment :: Environment } deriving (Generic)

result :: Text
result = show (formShape @Q)
"#;
    if let Some(msg) = rejection(decls) {
        assert_says(&msg, &["environment", "Environment", "Generic"]);
    }
}

/// An unsupported LEAF is the same failure as a missing `Generic`, and gets
/// the same message: from the outside there is no way to tell a scalar the
/// author wanted blessed from an ADT they forgot to derive.
///
/// Both corrections are the ones the message points at — derive `Generic` and
/// let the interpreter walk it, or end the field in `Text`, `Int`, `Double`,
/// or `Bool`.
#[test]
fn unsupported_leaf_names_the_field_and_the_type() {
    let decls = r#"
newtype Deadline = Deadline Int
data Q = Q { deadline :: Deadline } deriving (Generic)

result :: Text
result = show (formShape @Q)
"#;
    if let Some(msg) = rejection(decls) {
        assert_says(&msg, &["deadline", "Deadline", "Generic"]);
    }
}

/// The good case still compiles and runs: the checks are a floor under
/// unsupported shapes, not a tax on supported ones.
#[test]
fn supported_shapes_still_compile() {
    tidepool_testing::eval_harness::require_extract();
    let src = format!(
        "{HEADER}\n\
         data Env = Dev | Prod deriving (Generic)\n\
         data Q = Q {{ name :: Text, env :: Env, count :: Int, note :: Maybe Text }}\n\
         \x20 deriving (Generic)\n\n\
         result :: Text\n\
         result = show (formShape @Q)\n"
    );
    EvalHarness::new()
        .with_stdlib()
        .compile(&src, "result")
        .expect("a form over supported leaves must compile");
}
