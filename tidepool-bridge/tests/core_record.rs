//! Unit tests for the `CoreRecord` derive: exact Haskell-decl rendering for a
//! representative struct and enum, exercising the type map, snake→camel field
//! naming, `#[core(hs = ...)]` / `#[core(hs_type = ...)]` overrides, and the
//! `inventory` registration.
#![allow(dead_code)]

use tidepool_bridge::{all_record_decls, CoreRecord};
use tidepool_bridge_derive::CoreRecord;

// A nested record referenced by type name (`pos: SamplePos` → `SamplePos`).
#[derive(CoreRecord)]
#[core(name = "SamplePos")]
struct SamplePos {
    line: i64,
    character: i64,
}

// Exercises: field-name override (`hs`), whole-type override (`hs_type`),
// snake→camel default, Vec, Option, tuple, and a nested named type.
#[derive(CoreRecord)]
#[core(name = "Sample")]
struct Sample {
    #[core(hs = "sampleText")]
    text: String,
    exit_code: i64,
    ok: bool,
    ratio: f64,
    tags: Vec<String>,
    pairs: Vec<(String, String)>,
    note: Option<String>,
    counts: Option<i64>,
    #[core(hs = "samplePos", hs_type = "Position")]
    pos: SamplePos,
}

#[derive(CoreRecord)]
enum SampleEnum {
    #[core(name = "Rust")]
    Rust,
    #[core(name = "Python")]
    Python,
    #[core(name = "Tagged")]
    Tagged(i64, Option<String>),
}

#[test]
fn struct_haskell_decl_is_exact() {
    assert_eq!(
        Sample::haskell_decl(),
        "data Sample = Sample { sampleText :: Text, exitCode :: Int, ok :: Bool, \
         ratio :: Double, tags :: [Text], pairs :: [(Text, Text)], \
         note :: Maybe Text, counts :: Maybe Int, samplePos :: Position } \
         deriving (Show, Eq)"
    );
}

#[test]
fn nested_struct_decl_is_exact() {
    assert_eq!(
        SamplePos::haskell_decl(),
        "data SamplePos = SamplePos { line :: Int, character :: Int } deriving (Show, Eq)"
    );
}

#[test]
fn enum_haskell_decl_is_exact() {
    assert_eq!(
        SampleEnum::haskell_decl(),
        "data SampleEnum = Rust | Python | Tagged Int (Maybe Text) deriving (Show, Eq)"
    );
}

#[test]
fn records_register_in_inventory() {
    let decls = all_record_decls();
    // Sorted by Haskell type name → deterministic order.
    assert!(decls.iter().any(|d| d == &Sample::haskell_decl()));
    assert!(decls.iter().any(|d| d == &SamplePos::haskell_decl()));
    assert!(decls.iter().any(|d| d == &SampleEnum::haskell_decl()));
}
