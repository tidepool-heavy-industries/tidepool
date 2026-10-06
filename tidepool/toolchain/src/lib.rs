//! Locate, validate, fingerprint, and cache the Tidepool toolchain (the
//! `tidepool-extract` binary + the Haskell stdlib it pairs with) and its
//! compile outputs.
//!
//! Sits between `tidepool-extract-cmd` (the bound compiler endpoint and
//! invocation boundary this crate executes through) and `tidepool-runtime` (the high-level compile/run
//! API and session substrate, which depends on this crate and re-exports
//! what its own downstream callers still reach through `tidepool_runtime::`
//! paths).

use std::io;
use std::path::PathBuf;

use thiserror::Error;
use tidepool_repr::serial::ReadError;

pub mod activation_preview;
pub mod artifact_inventory;
pub mod artifacts;
pub mod cache;
pub mod cell_plan;
pub mod certified_products;
pub mod checked_cell;
mod compile_input;
pub use compile_input::CompileInputError;
mod declaration_context;
pub mod declaration_join;
pub mod diag;
pub mod digest;
mod execution_source;
pub mod failclass;
pub(crate) mod module_candidates;
pub mod paths;
pub mod prepared_artifact;
pub mod recovery_artifacts;
pub mod timing;
pub mod toolchain;
mod turn_observations;

pub use artifacts::{
    compile_targets, read_yield_sites, CompiledArtifacts, ConstructorIdentityMismatch, NominalHead,
    SiteType, TargetArtifact, YieldSite, YieldSiteCollision, YieldSites,
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
    /// Explicit immutable candidate configuration refused before compilation.
    #[error(transparent)]
    ModulePackage(#[from] crate::toolchain::ModulePackageError),
    /// I/O error during file operations or process execution.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    /// This original preparation was reserved before compiler execution. Its
    /// retained output must never be mistaken for permission to execute again.
    #[error("entry preparation {} is unfinished; retain its original output and select a fresh preparation identity to compile again", path.display())]
    EntryPreparationUnfinished { path: PathBuf },
    /// No source was submitted, but removing its reservation did not complete
    /// durably. A fresh reservation must first confirm the parent directory.
    #[error("entry reservation {} release is unconfirmed: {source}", path.display())]
    EntryReservationReleaseUnconfirmed {
        path: PathBuf,
        #[source]
        source: tidepool_atomic_write::WriteError,
    },
    /// Complete retained output became visible; source must not be re-executed
    /// to retry its parent-directory durability confirmation.
    #[error("entry {} is visible but durability is unconfirmed: {source}", path.display())]
    EntryPublicationUnconfirmed {
        path: PathBuf,
        #[source]
        source: tidepool_atomic_write::WriteError,
    },
    /// The extractor's typed output or an internal compile request violated
    /// its expected shape. Real GHC source rejections use `Diagnostics`; this
    /// variant therefore denotes an extractor/runtime contract mismatch.
    #[error("extractor contract failure: {0}")]
    ExtractFailed(String),
    /// Retained compiler evidence could not be read or decoded. Keep its
    /// resource and filesystem causes distinct from a compiler contract skew.
    #[error("compiler evidence rejected: {0}")]
    CompilerEvidence(#[source] Box<crate::certified_products::CertificationError>),
    #[error("artifact inventory: {0}")]
    ArtifactInventory(#[from] crate::artifact_inventory::ArtifactInventoryError),
    #[error("compiler input proof: {0}")]
    CompileInput(#[from] CompileInputError),
    /// Compiler request inputs were refused before GHC source checking.
    #[error("compiler input rejected ({} diagnostic(s))", .0.len())]
    InputRejected(Vec<crate::diag::ExtractDiag>),
    /// The extractor ran, exited non-zero, and its stdout parsed as a valid
    /// diagnostics report — this is a real GHC compile failure with real spans.
    #[error("Haskell compilation failed ({} diagnostic(s))", .0.len())]
    Diagnostics(Vec<crate::diag::ExtractDiag>),
    /// The accepted compiler-worker request failed outside GHC's
    /// `SourceError` path (for example an I/O failure, missing external tool,
    /// or internal exception). This is infrastructure, never authored code.
    #[error("compiler worker failed ({} diagnostic(s))", .0.len())]
    WorkerFailure(Vec<crate::diag::ExtractDiag>),
    /// The extractor's stdout did not parse as the diagnostics report (a
    /// stale binary predating the contract, or a genuine wire mismatch) — an
    /// infra/toolchain problem, not the user's Haskell.
    #[error("malformed extract diagnostics: {0}")]
    MalformedDiagnostics(String),
    /// Failed to deserialize the CBOR output from `tidepool-extract`.
    #[error("CBOR deserialization error: {0}")]
    ReadError(#[from] ReadError),
    /// Prepared execution bytes violated the exact schema/profile/target contract.
    #[error("prepared execution artifact rejected: {0}")]
    Prepared(#[from] tidepool_repr::execution_schema::ParseError),
    /// A required output file (.cbor or meta.cbor) was not produced by the extractor.
    #[error("Missing output file from extractor: {}", .0.display())]
    MissingOutput(PathBuf),
    /// The `asks.json` sidecar was present but did not parse.
    #[error("failed to parse asks.json: {0}")]
    Asks(String),
    /// The target binding has IO type, which is not supported.
    #[error("IO type detected in result binding. IO operations (unsafePerformIO, etc.) are not supported in the Tidepool sandbox.")]
    IOTypeDetected,
    /// A prepared program's constructor `host_id` RESOLVES in the
    /// `DataConTable` shipped alongside it, but to a different constructor —
    /// cross-paired artifact and metadata (one compile's prepared program
    /// with another compile's table), or an extractor identity-minting bug.
    /// Rejected at assembly so no handler ever interprets an observed
    /// `HaskellValue::Con(host_id, ...)` under the wrong table's nominal name. A
    /// `host_id` with no table entry at all is not this error — see
    /// `artifacts::check_constructor_identity_agreement`'s doc for why that
    /// is deliberately out of scope.
    #[error("constructor identity mismatch: {0}")]
    ConstructorIdentity(#[from] artifacts::ConstructorIdentityMismatch),
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
            "tidepool-extract not found on PATH (set TIDEPOOL_EXTRACT or install the Tidepool compiler tooling).",
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
