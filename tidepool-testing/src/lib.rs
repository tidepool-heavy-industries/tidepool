//! Testing utilities for Tidepool Core.
//!
//! This crate provides proptest generators for well-typed `CoreExpr` values,
//! enabling property-based testing of the Core representation, serialization,
//! and evaluation.
//!
//! # Integration-test harness
//!
//! New `tidepool-runtime` / `tidepool-repl` integration tests should build their
//! setup through [`eval_harness::EvalHarness`] rather than hand-rolling the
//! include path, the 8-256 MiB eval thread, the 10-effect GADT preamble, and the
//! mock handler stack. The harness wraps the real `tidepool_runtime` entry points
//! (`compile_and_run` / `compile_and_run_pure` / `compile_haskell`), so tests
//! keep driving the production compile→JIT→dispatch path. See that module's docs
//! for the pure / effectful / compile-only recipes.

pub mod compare;
pub mod dispatch;
pub mod eval_harness;
pub mod gen;
pub mod haskell_suite;
pub mod jit_run;
pub mod oracle;
pub mod proptest;
pub mod watchdog;

pub use dispatch::NullDispatcher;
