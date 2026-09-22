//! Shared test support for prepared-STG execution.
//!
//! # Integration-test harness
//!
//! New `tidepool-runtime` / `tidepool-repl` integration tests should build their
//! setup through [`eval_harness::EvalHarness`] rather than hand-rolling the
//! include path, the 8-256 MiB eval thread, the 13-effect GADT preamble, and the
//! mock handler stack. The harness wraps the real `tidepool_runtime` entry points
//! (`compile_and_run` / `compile_haskell`), so tests
//! keep driving the production compile→JIT→dispatch path. See that module's docs
//! for the effectful and compile-only recipes.

#![warn(clippy::unwrap_used, clippy::expect_used)]
pub mod effect_surface;
pub mod eval_harness;
