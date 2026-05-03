//! Harness for asserting equivalence between single-module and multi-module Haskell compilation.
//!
//! This catches bugs where GHC's post-optimizer Core shape diverges when bindings are
//! split across module boundaries (e.g. PR #272). Downstream tests use this to ensure
//! that any regression in cross-module translation is caught structurally.

use std::path::{Path, PathBuf};
use tempfile::TempDir;
use tidepool_runtime::{compile_haskell, DispatchEffect};
use tidepool_repr::{CoreExpr, DataConTable};

pub mod structural_eq;

/// A Haskell program expressed in two equivalent ways.
pub struct CrossModeFixture {
    /// Inlined source where all logic lives in one module.
    pub single: String,
    /// Split source where logic is divided across files.
    /// The `Vec` contains `(filename, source)` pairs.
    /// The LAST entry is the main module to be compiled.
    pub split: Vec<(String, String)>,
    /// The binder name to compile (e.g. "agent").
    pub target: &'static str,
}

/// Artifacts from both compilation modes.
pub struct CrossModeArtifacts {
    pub single_expr: CoreExpr,
    pub single_table: DataConTable,
    pub split_expr: CoreExpr,
    pub split_table: DataConTable,
    /// Keep the temp directory alive so include paths remain valid if needed
    /// (though CoreExpr/DataConTable are owned and don't need it).
    pub _temp_dir: Option<TempDir>,
}

/// Helper to get the path to the Haskell prelude library.
pub fn prelude_path() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().join("haskell").join("lib")
}

/// Compiles the fixture in both single and split modes.
pub fn compile_cross_mode(fixture: &CrossModeFixture) -> CrossModeArtifacts {
    let pp = prelude_path();

    // 1. Compile single mode
    let (single_expr, single_table, _) = compile_haskell(&fixture.single, fixture.target, &[&pp])
        .expect("failed to compile single-mode fixture");

    // 2. Compile split mode
    let temp_dir = TempDir::new().expect("failed to create temp dir for split-mode");
    let mut split_entries = fixture.split.iter().peekable();
    let mut last_source = String::new();

    while let Some((filename, source)) = split_entries.next() {
        if split_entries.peek().is_none() {
            // Last entry is the main module source
            last_source = source.clone();
        } else {
            // Other entries are dependencies to be written to the temp dir
            std::fs::write(temp_dir.path().join(filename), source)
                .expect("failed to write dependency file to temp dir");
        }
    }

    if last_source.is_empty() {
        panic!("fixture.split must contain at least one entry (the main module)");
    }

    let include = [pp.as_path(), temp_dir.path()];
    let (split_expr, split_table, _) = compile_haskell(&last_source, fixture.target, &include)
        .expect("failed to compile split-mode fixture");

    CrossModeArtifacts {
        single_expr,
        single_table,
        split_expr,
        split_table,
        _temp_dir: Some(temp_dir),
    }
}

/// Asserts that the fixture's single and split modes produce structurally equivalent Core trees.
///
/// 'Structural equivalence' means:
/// - Same tree shape (lockstep iteration over CoreFrames).
/// - Literals match exactly.
/// - DataConIds match by name + rep_arity lookup in their respective tables.
/// - VarIds are tolerated to differ (they are variants of hashed names).
pub fn assert_cross_mode_structurally_equivalent(fixture: &CrossModeFixture) {
    let artifacts = compile_cross_mode(fixture);
    structural_eq::assert_equivalent(&artifacts);
}

/// Asserts that the fixture's single and split modes produce equivalent runtime values for pure programs.
pub fn assert_cross_mode_pure_equivalent(fixture: &CrossModeFixture) {
    let pp = prelude_path();

    // Run single
    let res_single = tidepool_runtime::compile_and_run_pure(&fixture.single, fixture.target, &[&pp])
        .expect("failed to run single-mode fixture");

    // Run split
    let temp_dir = TempDir::new().expect("failed to create temp dir for split-mode");
    let mut split_entries = fixture.split.iter().peekable();
    let mut last_source = String::new();

    while let Some((filename, source)) = split_entries.next() {
        if split_entries.peek().is_none() {
            last_source = source.clone();
        } else {
            std::fs::write(temp_dir.path().join(filename), source)
                .expect("failed to write dependency file to temp dir");
        }
    }

    let include = [pp.as_path(), temp_dir.path()];
    let res_split = tidepool_runtime::compile_and_run_pure(&last_source, fixture.target, &include)
        .expect("failed to run split-mode fixture");

    structural_eq::assert_value_equivalent(
        res_single.value(),
        res_single.table(),
        res_split.value(),
        res_split.table(),
    );
}

/// Asserts that the fixture's single and split modes produce equivalent runtime values.
///
/// Runs both modes through the JIT and compares the resulting `Value`s recursively.
/// Constructor values are compared by name+arity.
#[allow(dead_code)]
pub fn assert_cross_mode_runtime_equivalent<U, H1, H2>(
    fixture: &CrossModeFixture,
    mk_single_handlers: impl FnOnce() -> H1,
    mk_split_handlers: impl FnOnce() -> H2,
    user: &U,
) where
    H1: DispatchEffect<U>,
    H2: DispatchEffect<U>,
{
    let pp = prelude_path();

    // Run single
    let mut h1 = mk_single_handlers();
    let res_single = tidepool_runtime::compile_and_run(&fixture.single, fixture.target, &[&pp], &mut h1, user)
        .expect("failed to run single-mode fixture");

    // Run split
    let temp_dir = TempDir::new().expect("failed to create temp dir for split-mode");
    let mut split_entries = fixture.split.iter().peekable();
    let mut last_source = String::new();

    while let Some((filename, source)) = split_entries.next() {
        if split_entries.peek().is_none() {
            last_source = source.clone();
        } else {
            std::fs::write(temp_dir.path().join(filename), source)
                .expect("failed to write dependency file to temp dir");
        }
    }

    let include = [pp.as_path(), temp_dir.path()];
    let mut h2 = mk_split_handlers();
    let res_split = tidepool_runtime::compile_and_run(&last_source, fixture.target, &include, &mut h2, user)
        .expect("failed to run split-mode fixture");

    structural_eq::assert_value_equivalent(
        res_single.value(),
        res_single.table(),
        res_split.value(),
        res_split.table(),
    );
}
