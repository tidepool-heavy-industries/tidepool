//! Locate, validate, fingerprint, and cache the Tidepool toolchain (the
//! `tidepool-extract` binary + the Haskell stdlib it pairs with) and its
//! compile outputs.
//!
//! Sits between `tidepool-extract-cmd` (the zero-dep invocation builder this
//! crate spawns through) and `tidepool-runtime` (the high-level compile/run
//! API and session substrate, which depends on this crate and re-exports
//! what its own downstream callers still reach through `tidepool_runtime::`
//! paths).

#![warn(clippy::unwrap_used, clippy::expect_used)]

use std::io;
use std::path::PathBuf;

use thiserror::Error;
use tidepool_repr::serial::ReadError;

pub mod artifacts;
pub mod cache;
pub mod diag;
pub mod failclass;
pub mod paths;
pub mod timing;
pub mod toolchain;

pub use artifacts::{
    compile_targets, compile_targets_with_session_inject, compile_targets_with_stable_inject,
    read_yield_sites, CompiledArtifacts, NominalHead, SessionInject, SiteType, StableValInject,
    TargetArtifact, YieldSite, YieldSiteCollision, YieldSites,
};
pub use failclass::{classify_compile, FailureClass, FailureEnvelope, Phase};

/// Errors that can occur during Haskell compilation via `tidepool-extract`.
/// The one error type both compile front doors ([`crate::compile_targets`]
/// and `tidepool_runtime::compile_haskell`) produce — re-exported at
/// `tidepool_runtime::CompileError` so every existing match site there
/// (session turn decoding, decl validation, `failclass::classify_compile`)
/// keeps compiling unchanged.
#[derive(Error, Debug)]
pub enum CompileError {
    /// I/O error during file operations or process execution.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    /// The extractor's typed output or an internal compile request violated
    /// its expected shape. Real GHC source rejections use `Diagnostics`; this
    /// variant therefore denotes an extractor/runtime contract mismatch.
    #[error("extractor contract failure: {0}")]
    ExtractFailed(String),
    /// The extractor ran, exited non-zero, and its stdout parsed as a valid
    /// diagnostics report — this is a real GHC compile failure with real spans.
    #[error("Haskell compilation failed ({} diagnostic(s))", .0.len())]
    Diagnostics(Vec<crate::diag::ExtractDiag>),
    /// The extractor's stdout did not parse as the diagnostics report (a
    /// stale binary predating the contract, or a genuine wire mismatch) — an
    /// infra/toolchain problem, not the user's Haskell.
    #[error("malformed extract diagnostics: {0}")]
    MalformedDiagnostics(String),
    /// Failed to deserialize the CBOR output from `tidepool-extract`.
    #[error("CBOR deserialization error: {0}")]
    ReadError(#[from] ReadError),
    /// A required output file (.cbor or meta.cbor) was not produced by the extractor.
    #[error("Missing output file from extractor: {}", .0.display())]
    MissingOutput(PathBuf),
    /// The `asks.json` sidecar was present but did not parse.
    #[error("failed to parse asks.json: {0}")]
    Asks(String),
    /// The target binding has IO type, which is not supported.
    #[error("IO type detected in result binding. IO operations (unsafePerformIO, etc.) are not supported in the Tidepool sandbox.")]
    IOTypeDetected,
}

/// Rewrite an extractor spawn failure into a self-explaining `io::Error`:
/// `NotFound` means the `tidepool-extract` binary is missing or misconfigured —
/// an environment problem ([`FailureClass::Infra`]), never the user's Haskell.
/// Every extractor spawn site maps through here so the message and
/// classification stay uniform.
pub fn extract_spawn_error(e: io::Error) -> io::Error {
    if e.kind() == io::ErrorKind::NotFound {
        io::Error::new(
            io::ErrorKind::NotFound,
            "tidepool-extract not found on PATH (set TIDEPOOL_EXTRACT or install the Tidepool harness).",
        )
    } else {
        e
    }
}

/// Extract module name from Haskell source (e.g. "module Expr where" -> "Expr").
pub fn extract_module_name(source: &str) -> Option<String> {
    for line in source.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("module ") {
            // "module Foo.Bar where" or "module Foo (" → take until whitespace/paren
            let name: String = rest
                .trim_start()
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '.' || *c == '_')
                .collect();
            if !name.is_empty() {
                return Some(name);
            }
        }
    }
    None
}
