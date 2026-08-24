//! `tidepool-compile-report` — folds durable compile-failure evidence
//! (a self-iterating harness's `transcript.jsonl`, and this project's own
//! eval-surface `eval-failures.jsonl`) into a ranked desire-path report:
//! which unsupported constructs are most reached for, by how much, and
//! whether first-try-compile rate is trending up or down run over run.
//!
//! See `tidepool::compile_report` for the fold itself and
//! `plans/flight-dogfood-campaign.md`'s cadence section for when to run this.
//!
//! ```text
//! tidepool-compile-report <path-or-glob>...
//! tidepool-compile-report --format json <path-or-glob>...
//! ```
//!
//! Each argument is either a literal file path or a glob pattern (`glob`
//! crate syntax, e.g. `~/.cache/tidepool/selfharness/transcript.jsonl` or
//! `scratchpad/flight-rounds/*/transcript.jsonl`) — every matched file is
//! read as one evidence source and contributes one row to the per-run trend.
//! A path that names no existing file and matches no glob is reported and
//! skipped, never silently dropped.

#![warn(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use clap::{Parser, ValueEnum};
use tidepool::compile_report::{build_report, read_evidence_file, RunEvidence};

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Format {
    Human,
    Json,
}

/// Fold durable compile-failure evidence into a ranked desire-path report.
#[derive(Parser)]
struct Args {
    /// Literal file paths or glob patterns naming evidence files
    /// (`transcript.jsonl` / `eval-failures.jsonl`). At least one required.
    #[arg(required = true)]
    paths: Vec<String>,
    /// Output format.
    #[arg(long, value_enum, default_value_t = Format::Human)]
    format: Format,
}

fn expand(pattern: &str) -> Vec<PathBuf> {
    let literal = PathBuf::from(pattern);
    if literal.is_file() {
        return vec![literal];
    }
    match glob::glob(pattern) {
        Ok(paths) => paths
            .filter_map(Result::ok)
            .filter(|p| p.is_file())
            .collect(),
        Err(e) => {
            eprintln!("warning: {pattern:?} is not a valid glob pattern: {e}");
            Vec::new()
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let mut files: Vec<PathBuf> = Vec::new();
    for pattern in &args.paths {
        let matched = expand(pattern);
        if matched.is_empty() {
            eprintln!("warning: {pattern:?} matched no existing file — skipped");
        }
        files.extend(matched);
    }
    files.sort();
    files.dedup();

    if files.is_empty() {
        eprintln!("no evidence files matched any of: {:?}", args.paths);
        std::process::exit(1);
    }

    let mut runs: Vec<RunEvidence> = Vec::with_capacity(files.len());
    for path in &files {
        match read_evidence_file(path) {
            Ok(ev) => runs.push(ev),
            Err(e) => eprintln!("warning: skipping {path:?}: {e}"),
        }
    }

    let report = build_report(&runs);
    match args.format {
        Format::Human => print!("{}", report.to_human()),
        Format::Json => println!("{}", report.to_json()),
    }

    Ok(())
}
