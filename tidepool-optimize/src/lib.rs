//! Optimization passes for Tidepool Core expressions.
//!
//! Includes beta reduction, case reduction, dead code elimination, inlining,
//! occurrence analysis, and partial evaluation.

pub mod beta;
pub mod case_reduce;
pub mod dce;
pub mod inline;
pub mod occ;
pub mod partial;
pub mod pipeline;
mod rewrite;

pub use pipeline::{default_passes, optimize, run_pipeline, PipelineStats};
