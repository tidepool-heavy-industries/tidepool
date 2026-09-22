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
//! CLI entry point.
//!
//! **When to run it.** After any long dogfood/harness session, and
//! periodically against the live evidence logs outside a session, to catch
//! drift. The ranked bucket counts are raw material for a human aggregate
//! step, not a replacement for it: the report ranks WHAT broke, a person
//! still judges WHY and what to do about it.
//!
//! **Ranked entry -> pave-or-dam decision.** A bucket's top identifiers are
//! candidate constructs to either PAVE (add real support — a missing stdlib
//! function, a new effect, a JIT primop) or DAM (the model is reaching for
//! something that should not exist here — tighten the prompt/docs to steer
//! away from it instead). Which one depends on the identifier, not the
//! bucket: `variable-not-in-scope` naming a real stdlib gap is a pave; the
//! same bucket naming a hallucinated verb from a different codebase's
//! vocabulary is a dam. A bucket with a persistently non-zero count and the
//! SAME top identifier across multiple independent runs is the strong
//! signal — a one-off in a single run is noise, a repeat across several runs
//! is a desire path. `other` growing without a clear identifier pattern is
//! itself a finding: the classifier's bucket set no longer covers what is
//! actually failing.
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
