//! The compile-failure report: folds durable compile-failure evidence
//! (a self-iterating harness's `transcript.jsonl`, this crate's own
//! eval-surface `eval-failures.jsonl`) into a ranked "most-reached-for-but-
//! unsupported constructs" report — variable/type-constructor/module scope
//! misses, missing instances, JIT gaps, import-grammar rejections, and
//! wrapper-attributed failures, each with its most-named identifiers and a
//! per-run first-try-compile-rate trend.
//!
//! This exists so surface evolution (root `CLAUDE.md`: "the interface
//! evolves as an optimization loop") is driven by counted, ranked evidence
//! instead of an anecdote from the last dogfood round. See
//! `tidepool-compile-report` (`src/bin/tidepool-compile-report.rs`) for the
//! CLI entry point, and `plans/flight-dogfood-campaign.md`'s "Compile-failure
//! report cadence" section for when to run it.
//!
//! **Placement note** (flagged per this feature's spec): this logic lives in
//! the facade crate rather than `tidepool-harness` (whose `transcript.jsonl`
//! schema is the primary evidence source) because the harness crate was
//! off-limits to modify for this change (a concurrent fork lane), and rather
//! than `tidepool-repr` (the crate that owns the durable-JSONL primitive this
//! reads through) because that crate's charter is IR/wire-format only, not
//! compile-diagnostics domain. If `tidepool-harness`'s `observer::Event` ever
//! grows a `Deserialize` impl, `read.rs`'s hand-decoded `serde_json::Value`
//! parsing should be replaced with that real type directly.
pub mod classify;
pub mod read;
pub mod report;

pub use classify::{classify_error, Classification, DesireBucket};
pub use read::{
    read_evidence_file, AnswererRoundRow, EvalFailureRow, EvidenceReadError, RunEvidence,
};
pub use report::{build_report, BucketReportRow, CompileFailureReport, RunTrendRow};
