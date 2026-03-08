//! Testing utilities for Tidepool Core.
//!
//! This crate provides proptest generators for well-typed `CoreExpr` values,
//! enabling property-based testing of the Core representation, serialization,
//! and evaluation.

pub mod compare;
pub mod gen;
pub mod oracle;
