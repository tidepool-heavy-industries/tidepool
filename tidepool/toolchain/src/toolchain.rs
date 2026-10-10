//! The ONE toolchain locator — where the GHC→prepared-STG extractor and the
//! Haskell stdlib source tree live — plus the startup **handshake** that
//! refuses to serve an extract/stdlib pair that was not deployed together.
//!
//! The extract precedence (`$TIDEPOOL_EXTRACT` + `$PATH` fallback) and the
//! stdlib precedence (`$TIDEPOOL_PRELUDE_DIR`, the `dist-newstyle` sibling
//! walk, the cwd/bundle search) are consolidated into one place so they
//! cannot disagree. A disagreement between separate policies surfaces as a
//! *wrong answer at eval time* ("not in scope", "Metadata entry must be an
//! array of exactly 9") rather than as a configuration error. Both
//! precedence orders live here, once, and both are documented below.
//!
//! # Precedence: the extract binary
//!
//! Owned by `tidepool-extract-cmd` (`resolve_bin`), the crate that also builds
//! the extract COMMAND — location and invocation of the same binary belong
//! together. Reproduced here because the stdlib table below depends on it.
//!
//! | # | Source | Notes |
//! |---|--------|-------|
//! | 1 | `$TIDEPOOL_EXTRACT` | Explicit override, and STRICT: set-but-unreadable is a hard error, never a silent fall-through to `$PATH` — falling through would run a different binary than the caller believes it is running. |
//! | 2 | `tidepool-extract` on `$PATH` | Normally `~/.nix-profile/bin/tidepool-extract`, a wrapper that prepends the with-packages GHC and `exec`s the store binary. |
//!
//! [`extract_command_name`] returns the frontend selection;
//! [`bind_extract_endpoint`] binds the exact producer and its reported
//! identity, while [`locate_extract`] supplies an informational absolute path.
//! All honor row 1's STRICT clause — a
//! set-but-unreadable `$TIDEPOOL_EXTRACT` is a hard error out of either, never
//! a silent fall-through to row 2.
//!
//! # Precedence: the Haskell stdlib source root
//!
//! The stdlib root is the GHC include dir under which `Tidepool/Prelude.hs`
//! lives. Every step is checked with [`is_stdlib_root`]; a step that names a
//! directory without `Tidepool/Prelude.hs` does not count as a hit.
//!
//! | # | Source | Rationale |
//! |---|--------|-----------|
//! | 1 | `$TIDEPOOL_PRELUDE_DIR` | Operator override. **Set-but-not-a-stdlib-root is a hard error**, never a silent fall-through — a typo'd override that quietly served a different stdlib is exactly the failure this module exists to kill. |
//! | 2 | `./bridge/haskell/lib`, then `./lib` (from CWD) | In-repo development: the working tree you are editing wins over anything installed. Preserves the `tidepool` binary's historical behavior. |
//! | 3 | Sibling of the extract's `dist-newstyle` | Absorbs the old `derive_stdlib_include`: walk `$TIDEPOOL_EXTRACT` up to a `dist-newstyle` component and take its sibling `lib/`. Pairs a worktree-built extract with that worktree's stdlib. |
//! | 4 | [`StdlibFallbacks::bundle`] | Installed mode: the stdlib embedded in the server binary, materialized to a content-addressed cache dir. Immutable and guaranteed to match the binary. Only release builds (`TIDEPOOL_EMBED_HASKELL=1`) embed anything here; a dev build's bundle is an empty materialized directory that never satisfies [`is_stdlib_root`], so this step falls through and step 2 (or 3) resolves instead. |
//! | 5 | [`StdlibFallbacks::build_tree`] | Last resort: the source tree this binary was *built* from (`env!("CARGO_MANIFEST_DIR")`-derived). Keeps a repo-installed `tidepool-repl` working when launched outside the repo. |
//! | — | otherwise | [`ToolchainError::StdlibNotFound`], listing every path tried. |
//!
//! # The handshake
//!
//! Deploy coupling — extract, both servers, and the stdlib must move together
//! (`scripts/redeploy.sh`) — is checked at startup:
//!
//! - `scripts/redeploy.sh` finishes by running `tidepool --write-toolchain-stamp`,
//!   which records the bound producer identity and the content fingerprint of the
//!   stdlib tree it just deployed into [`stamp_path`].
//! - Each server calls [`enforce_handshake`] once at startup. It binds the
//!   actual producer, fingerprints the resolved stdlib, and compares them to the stamp.
//!   A mismatch means one side moved without the other → loud, actionable
//!   failure naming `scripts/redeploy.sh`.
//!
//! Fingerprints are **content-only, never paths**: the stamp is written from
//! the repo (stdlib = `bridge/haskell/lib`) but checked from a server whose stdlib is
//! the materialized bundle at a completely different path. Identical content
//! must compare equal.
//!
//! Cost: one endpoint preflight plus one complete shipped-source walk. Never
//! per-eval.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Env var naming the extract binary (step 1 of the extract precedence).
/// Read here only for the stdlib table's step 3; the extract binary itself is
/// resolved by [`tidepool_extract_cmd::resolve_bin`].
pub const ENV_EXTRACT: &str = "TIDEPOOL_EXTRACT";
/// Env var naming the private compiler worker used by the Rust frontend.
pub const ENV_EXTRACT_WORKER: &str = "TIDEPOOL_EXTRACT_WORKER";
/// Env var pointing at the configured producer and consumed-worker manifest.
pub const ENV_COMPILER_DEPLOYMENT: &str = "TIDEPOOL_COMPILER_DEPLOYMENT";
/// Optional immutable source/product catalog paired with configured compiler authority.
pub const ENV_COMPILER_MODULES: &str = "TIDEPOOL_COMPILER_MODULES";
pub use crate::module_candidates::deployment::{
    DeploymentModulePackage, ModulePackageError, NativeCatalogSourceSelection, NativeSourceRole,
};

/// Select fresh acquisition or continuation of an explicitly retained catalog.
/// An acquired absent selection never falls back to later ambient configuration.
#[derive(Clone, Debug, Default)]
pub enum CatalogSelection {
    #[default]
    FreshConfigured,
    Acquired(Option<std::sync::Arc<DeploymentModulePackage>>),
}

impl CatalogSelection {
    pub fn for_deployment(
        &self,
        deployment: &AdmittedCompilerDeployment,
    ) -> Result<Self, ModulePackageError> {
        let package = self.acquire()?;
        if let Some(package) = &package {
            package.validate_deployment(deployment)?;
        }
        Ok(Self::Acquired(package))
    }

    pub fn acquire(
        &self,
    ) -> Result<Option<std::sync::Arc<DeploymentModulePackage>>, ModulePackageError> {
        match self {
            Self::FreshConfigured => configured_module_package(),
            Self::Acquired(package) => Ok(package.clone()),
        }
    }
}

/// Authenticate a new configured acquisition. Retained compiler consumers pass
/// their acquired owner explicitly rather than reacquiring through this entry.
pub fn configured_module_package(
) -> Result<Option<std::sync::Arc<DeploymentModulePackage>>, ModulePackageError> {
    crate::host_work::checkpoint().map_err(ModulePackageError::Interrupted)?;
    let Some(path) = std::env::var_os(ENV_COMPILER_MODULES) else {
        return Ok(None);
    };
    let configuration = CompilerDeploymentConfiguration::from_env()
        .map_err(|e| ModulePackageError::CompilerConfiguration(Box::new(e)))?;
    let CompilerDeploymentConfiguration::Configured(authority) = configuration else {
        return Err(ModulePackageError::UnknownCompiler);
    };
    DeploymentModulePackage::load(&PathBuf::from(path), &authority)
        .map(std::sync::Arc::new)
        .map(Some)
}

/// Validate configured source provenance without hydrating native products.
/// Candidate selection authenticates the complete package, retaining its
/// decoded objects under the explicitly acquired package owner.
pub fn configured_module_source_selection(
) -> Result<Option<NativeCatalogSourceSelection>, ModulePackageError> {
    let Some(path) = std::env::var_os(ENV_COMPILER_MODULES) else {
        return Ok(None);
    };
    let configuration = CompilerDeploymentConfiguration::from_env()
        .map_err(|error| ModulePackageError::CompilerConfiguration(Box::new(error)))?;
    let CompilerDeploymentConfiguration::Configured(authority) = configuration else {
        return Err(ModulePackageError::UnknownCompiler);
    };
    DeploymentModulePackage::load_source_selection(&PathBuf::from(path), &authority).map(Some)
}

/// Env var naming the stdlib root (step 1 of the stdlib precedence).
pub const ENV_PRELUDE_DIR: &str = "TIDEPOOL_PRELUDE_DIR";
/// Env var overriding [`stamp_path`].
pub const ENV_STAMP: &str = "TIDEPOOL_TOOLCHAIN_STAMP";
/// Env var selecting the handshake severity: `error` (default) / `warn` / `off`.
pub const ENV_HANDSHAKE: &str = "TIDEPOOL_TOOLCHAIN_HANDSHAKE";

/// The deploy command every skew message points at.
const REDEPLOY: &str = "scripts/redeploy.sh";

/// The executable role identified by a no-input Tidepool extractor probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExtractBinaryRole {
    Frontend,
    Worker,
    Unknown,
}

/// Classify the stable no-input response of an extractor executable.
#[must_use]
pub fn classify_extract_binary(stderr: &[u8]) -> ExtractBinaryRole {
    if stderr.starts_with(b"Usage: tidepool-extract [") {
        ExtractBinaryRole::Frontend
    } else if stderr.starts_with(b"worker requires") {
        ExtractBinaryRole::Worker
    } else {
        ExtractBinaryRole::Unknown
    }
}

/// Probe an extractor executable without permitting either binary role to
/// stand in for the other.
#[must_use]
pub fn probe_extract_binary(path: &Path) -> ExtractBinaryRole {
    tidepool_extract_cmd::probe_binary(path)
        .map(|output| classify_extract_binary(&output.stderr))
        .unwrap_or(ExtractBinaryRole::Unknown)
}

/// A repository-local frontend and compiler worker validated as a pair.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DevelopmentExtractPair {
    frontend: PathBuf,
    worker: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum DevelopmentExtractError {
    #[error("{ENV_EXTRACT_WORKER}={} is not a Tidepool compiler worker", .0.display())]
    InvalidWorkerOverride(PathBuf),
    #[error("could not locate the Cabal compiler worker: {0}")]
    LocateWorker(#[source] std::io::Error),
    #[error("`cabal list-bin tidepool-extract-bin` failed with {0}")]
    CabalFailed(std::process::ExitStatus),
    #[error("Cabal resolved {} but it is not a Tidepool compiler worker", .0.display())]
    InvalidCabalWorker(PathBuf),
}

impl DevelopmentExtractPair {
    /// Locate an already-built pair in a Tidepool checkout. This never builds
    /// either half and returns `None` when the frontend is not ready.
    ///
    /// # Errors
    /// Returns a typed error when the frontend exists but its explicit or
    /// Cabal-resolved worker does not satisfy the worker contract.
    pub fn discover(repo_root: &Path) -> Result<Option<Self>, DevelopmentExtractError> {
        let frontend = repo_root.join("target/debug/tidepool-extract");
        if probe_extract_binary(&frontend) != ExtractBinaryRole::Frontend {
            return Ok(None);
        }

        let worker = match std::env::var_os(ENV_EXTRACT_WORKER) {
            Some(path) => {
                let path = PathBuf::from(path);
                if probe_extract_binary(&path) != ExtractBinaryRole::Worker {
                    return Err(DevelopmentExtractError::InvalidWorkerOverride(path));
                }
                path
            }
            None => {
                #[allow(
                    clippy::disallowed_methods,
                    reason = "short synchronous probe: `cabal list-bin` exits immediately and is not a long-lived child"
                )]
                let output = Command::new("cabal")
                    .args(["list-bin", "tidepool-extract-bin"])
                    .current_dir(repo_root.join("bridge/haskell"))
                    .output()
                    .map_err(DevelopmentExtractError::LocateWorker)?;
                if !output.status.success() {
                    return Err(DevelopmentExtractError::CabalFailed(output.status));
                }
                let path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
                if probe_extract_binary(&path) != ExtractBinaryRole::Worker {
                    return Err(DevelopmentExtractError::InvalidCabalWorker(path));
                }
                path
            }
        };

        Ok(Some(Self { frontend, worker }))
    }

    /// Publish the worker before the frontend, making the frontend assignment
    /// the point at which the validated pair becomes available to callers.
    pub fn install(self) {
        std::env::set_var(ENV_EXTRACT_WORKER, self.worker);
        std::env::set_var(ENV_EXTRACT, self.frontend);
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// A toolchain *configuration* failure: the extract or the stdlib could not be
/// located, or the located pair is skewed. Distinct from a compile failure —
/// nothing the user's Haskell can cause.
#[derive(thiserror::Error)]
pub enum ToolchainError {
    /// No runnable extract: `$TIDEPOOL_EXTRACT` named an unreadable file, or
    /// the bare name is not on `$PATH`.
    #[error(
        "tidepool-extract not found ({tried}). Set {ENV_EXTRACT} to a built \
         tidepool-extract frontend from the native runtime bundle."
    )]
    ExtractNotFound {
        /// What was searched for, as the locator crate reported it.
        tried: String,
    },

    /// `$TIDEPOOL_PRELUDE_DIR` is set but does not name a stdlib root.
    #[error(
        "{ENV_PRELUDE_DIR}={} is not a Tidepool stdlib root (no Tidepool/Prelude.hs under it). \
         Point it at a directory containing Tidepool/Prelude.hs, or unset it to use the \
         bundled stdlib.",
        .dir.display()
    )]
    PreludeDirInvalid {
        /// The offending override.
        dir: PathBuf,
    },

    /// No step of the stdlib precedence found a root.
    #[error(
        "Tidepool stdlib not found — no Tidepool/Prelude.hs under any of: {}. \
         Set {ENV_PRELUDE_DIR}, run from a Tidepool checkout, or reinstall the server \
         (`{REDEPLOY}`).",
        render_tried(.tried)
    )]
    StdlibNotFound {
        /// Every (precedence step, path) pair that was checked, in order.
        tried: Vec<(&'static str, PathBuf)>,
    },

    /// The located extract and stdlib were not deployed together. Boxed: the
    /// report carries both fingerprints plus the whole stamp, and this error
    /// rides in `Result`s on hot session paths where an unboxed 200+ byte
    /// variant would widen every `Ok` too (clippy's `result_large_err`).
    #[error("{0}")]
    Skew(Box<SkewReport>),

    /// The stdlib tree could not be completely inspected.
    #[error("stdlib fingerprint: {0}")]
    StdlibManifest(#[from] crate::cache::SourceManifestError),

    /// Reading or writing the deploy stamp failed.
    #[error("toolchain stamp {}: {source}", .path.display())]
    Stamp {
        /// The stamp path involved.
        path: PathBuf,
        /// Underlying I/O or JSON failure.
        source: std::io::Error,
    },

    /// A configured compiler deployment is absent, malformed, or does not
    /// authorize the producer and worker observed at the endpoint.
    #[error("compiler deployment authority rejected the bound endpoint: {0}")]
    DeploymentAuthority(#[from] DeploymentAdmissionError),

    /// Reading or parsing the configured deployment manifest failed.
    #[error("compiler deployment manifest {}: {source}", .path.display())]
    DeploymentManifest {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// `Debug` renders the variant name plus the operator-facing `Display` text,
/// NOT a field dump.
///
/// This error reaches `main()` in both server binaries, and Rust prints a
/// returned `Err` with `Debug` — a derived `Debug` there spilled the whole
/// `SkewReport` struct (every fingerprint, the entire stamp) and buried the
/// one sentence saying to run `scripts/redeploy.sh`. The struct dump had no
/// consumer; the message has one.
impl std::fmt::Debug for ToolchainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tag = match self {
            Self::ExtractNotFound { .. } => "ExtractNotFound",
            Self::PreludeDirInvalid { .. } => "PreludeDirInvalid",
            Self::StdlibNotFound { .. } => "StdlibNotFound",
            Self::Skew(_) => "Skew",
            Self::Stamp { .. } => "Stamp",
            Self::StdlibManifest(_) => "StdlibManifest",
            Self::DeploymentAuthority(_) => "DeploymentAuthority",
            Self::DeploymentManifest { .. } => "DeploymentManifest",
        };
        write!(f, "{tag}: {self}")
    }
}

fn render_tried(tried: &[(&'static str, PathBuf)]) -> String {
    tried
        .iter()
        .map(|(what, p)| format!("{what} ({})", p.display()))
        .collect::<Vec<_>>()
        .join(", ")
}

// ---------------------------------------------------------------------------
// Extract location — delegated
// ---------------------------------------------------------------------------

/// The resolved extract binary, typed so a caller cannot substitute a
/// guessed name for a real resolution.
///
/// Deliberately spelled without naming the std spawn constructor: this helper
/// resolves selection, while endpoint binding and execution remain in
/// `tidepool-extract-cmd`. The `no_open_coded_extract_spawns` guard is a source
/// scan that (correctly) cannot tell prose from code.
///
/// Thin delegation to [`tidepool_extract_cmd::resolve_bin`], which owns the
/// extract-binary precedence (see the table in this module's docs). Kept as a
/// named entry point so a caller that only needs the resolved binary — the
/// harness's `EngineConfig` — does not have to reach into the invocation
/// crate.
///
/// STRICT, per the precedence table: a set-but-unreadable `$TIDEPOOL_EXTRACT`
/// is a hard error here too, never a silent fall-through to the bare
/// [`tidepool_extract_cmd::DEFAULT_BIN`] name — a caller that received the
/// bare name back would spawn a DIFFERENT binary than the override named, and
/// believe it was running the one it configured. An UNSET env falls back to
/// the bare name unchanged (`Ok`, not an error — [`ToolchainError::ExtractNotFound`]
/// is for [`locate_extract`]'s PATH-lookup failure, a distinct case).
///
/// # Errors
/// [`ToolchainError::ExtractNotFound`] when `$TIDEPOOL_EXTRACT` names an
/// unreadable file, naming the path, why it failed, and that unsetting the
/// var falls back to `$PATH`.
pub fn extract_command_name() -> Result<tidepool_extract_cmd::ResolvedExtractBin, ToolchainError> {
    tidepool_extract_cmd::resolve_bin()
        .map(tidepool_extract_cmd::ResolvedBin::into_extract_bin)
        .map_err(|e| ToolchainError::ExtractNotFound {
            tried: format!(
                "{ENV_EXTRACT} is set to {} but that is not a readable file; \
                 unset {ENV_EXTRACT} to fall back to $PATH lookup",
                e.path().display()
            ),
        })
}

/// A resolved extract binary: an ABSOLUTE path.
#[derive(Debug, Clone)]
pub struct ExtractLocation {
    /// Absolute path to the binary (or wrapper script) — resolved through
    /// `PATH` when `$TIDEPOOL_EXTRACT` is unset, so it is always a real file
    /// used for diagnostics and stdlib sibling discovery.
    pub path: PathBuf,
}

/// Resolve the extract to a real file on disk, typed-failing when there is
/// none.
///
/// [`tidepool_extract_cmd::resolve_bin`] decides WHICH binary; this adds the
/// `PATH` lookup its `PathLookup` case defers to the OS, because the handshake
/// and the degraded-setup probe need an absolute path, not a name to spawn.
///
/// # Errors
/// [`ToolchainError::ExtractNotFound`] when `$TIDEPOOL_EXTRACT` names an
/// unreadable file, or when the bare name is not on `$PATH`.
pub fn locate_extract() -> Result<ExtractLocation, ToolchainError> {
    let resolved =
        tidepool_extract_cmd::resolve_bin().map_err(|e| ToolchainError::ExtractNotFound {
            tried: e.to_string(),
        })?;
    // An Env-sourced path is already known-readable; a PathLookup one is the
    // bare name and still needs the OS search.
    which::which(&resolved.path)
        .map(|path| ExtractLocation { path })
        .map_err(|_| ToolchainError::ExtractNotFound {
            tried: format!("{} on $PATH", resolved.path.display()),
        })
}

/// Bind the producer selected by the canonical extract resolution policy.
/// Configured deployment authority admits the observed producer and exact
/// consumed worker before this endpoint can authorize compilation.
pub fn bind_extract_endpoint(
) -> Result<(tidepool_extract_cmd::CompilerEndpoint, ExtractLocation), ToolchainError> {
    let (endpoint, location, _) =
        bind_admitted_extract_endpoint(&CompilerDeploymentConfiguration::from_env()?)?;
    Ok((endpoint, location))
}
fn bind_unadmitted_extract_endpoint(
) -> Result<(tidepool_extract_cmd::CompilerEndpoint, ExtractLocation), ToolchainError> {
    let location = locate_extract()?;
    let cmd = tidepool_extract_cmd::ExtractCmd::with_bin(
        tidepool_extract_cmd::ResolvedExtractBin::assume_resolved(&location.path),
    );
    let endpoint = cmd
        .bind()
        .map_err(|error| ToolchainError::ExtractNotFound {
            tried: error.to_string(),
        })?;
    Ok((endpoint, location))
}

/// Configured evidence for one compiler deployment. These digests originate
/// from the configured package/deployment manifest; they must never be filled
/// from an endpoint observation and then treated as authority.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompilerDeploymentAuthority {
    /// Schema version of this explicit deployment manifest.
    pub schema: u32,
    /// Exact producer identity emitted by the configured frontend artifact.
    pub producer_identity: [u8; 32],
    /// BLAKE3 digest of the exact compiler worker executable admitted into it.
    pub consumed_worker_identity: [u8; 32],
    /// Configured frontend artifact location and build provenance.
    pub frontend_path: PathBuf,
    /// Configured worker artifact location and build provenance.
    pub worker_path: PathBuf,
    /// GHC library directory hashed into the configured producer identity.
    pub ghc_libdir: PathBuf,
}

/// Explicitly selected deployment mode. Local callers can supply a configured
/// test/development authority directly; an unknown or missing production
/// authority fails closed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompilerDeploymentConfiguration {
    Configured(CompilerDeploymentAuthority),
    Unknown,
}

/// Evidence retained after the configured deployment has admitted an
/// observed endpoint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmittedCompilerDeployment {
    /// Producer identity validated against the configured deployment.
    pub producer_identity: [u8; 32],
    /// Exact worker identity validated against the configured deployment.
    pub consumed_worker_identity: [u8; 32],
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DeploymentAdmissionError {
    #[error("no configured deployment authority is available")]
    Unknown,
    #[error("configured deployment manifest has unsupported schema {0}")]
    Schema(u32),
    #[error("configured deployment manifest has empty producer or worker identity")]
    EmptyIdentity,
    #[error("configured deployment manifest requires absolute frontend, worker, and GHC paths")]
    InvalidPath,
    #[error("bound producer differs from configured deployment")]
    ProducerMismatch,
    #[error("consumed worker differs from configured deployment")]
    WorkerMismatch,
}

impl CompilerDeploymentConfiguration {
    fn validate(&self) -> Result<(), DeploymentAdmissionError> {
        match self {
            Self::Unknown => Err(DeploymentAdmissionError::Unknown),
            Self::Configured(authority) if authority.schema != 1 => {
                Err(DeploymentAdmissionError::Schema(authority.schema))
            }
            Self::Configured(authority)
                if authority.producer_identity == [0; 32]
                    || authority.consumed_worker_identity == [0; 32] =>
            {
                Err(DeploymentAdmissionError::EmptyIdentity)
            }
            Self::Configured(authority)
                if !authority.frontend_path.is_absolute()
                    || !authority.worker_path.is_absolute()
                    || !authority.ghc_libdir.is_absolute() =>
            {
                Err(DeploymentAdmissionError::InvalidPath)
            }
            Self::Configured(_) => Ok(()),
        }
    }

    /// Load configured production authority from the manifest named by
    /// `$TIDEPOOL_COMPILER_DEPLOYMENT`. An unset variable remains explicitly
    /// unknown; local callers can pass an explicit test/development authority.
    pub fn from_env() -> Result<Self, ToolchainError> {
        let Some(path) = std::env::var_os(ENV_COMPILER_DEPLOYMENT) else {
            return Ok(Self::Unknown);
        };
        let path = PathBuf::from(path);
        let bytes = std::fs::read(&path).map_err(|source| ToolchainError::DeploymentManifest {
            path: path.clone(),
            source,
        })?;
        let authority: CompilerDeploymentAuthority =
            serde_json::from_slice(&bytes).map_err(|e| ToolchainError::DeploymentManifest {
                path: path.clone(),
                source: std::io::Error::new(std::io::ErrorKind::InvalidData, e),
            })?;
        let configuration = Self::Configured(authority);
        configuration.validate()?;
        Ok(configuration)
    }

    /// Admit the exact identity reported by the bound endpoint. Producer and
    /// worker are checked independently so changing the consumed worker cannot
    /// hide behind an unchanged configured frontend identity.
    pub fn admit(
        &self,
        producer_identity: [u8; 32],
        consumed_worker_identity: [u8; 32],
    ) -> Result<AdmittedCompilerDeployment, DeploymentAdmissionError> {
        self.validate()?;
        match self {
            Self::Unknown => Err(DeploymentAdmissionError::Unknown),
            Self::Configured(authority) => {
                if producer_identity != authority.producer_identity {
                    return Err(DeploymentAdmissionError::ProducerMismatch);
                }
                if consumed_worker_identity != authority.consumed_worker_identity {
                    return Err(DeploymentAdmissionError::WorkerMismatch);
                }
                Ok(AdmittedCompilerDeployment {
                    producer_identity,
                    consumed_worker_identity,
                })
            }
        }
    }
}

/// Bind and admit a compiler endpoint under an explicit configured deployment.
/// Production callers should pass `CompilerDeploymentConfiguration::from_env()`
/// and handle `Unknown` as refusal.
pub fn bind_admitted_extract_endpoint(
    configuration: &CompilerDeploymentConfiguration,
) -> Result<
    (
        tidepool_extract_cmd::CompilerEndpoint,
        ExtractLocation,
        AdmittedCompilerDeployment,
    ),
    ToolchainError,
> {
    configuration.validate()?;
    let (endpoint, location) = bind_unadmitted_extract_endpoint()?;
    let admitted = configuration.admit(
        *endpoint.identity().producer_bytes(),
        *endpoint.identity().consumed_worker_bytes(),
    )?;
    Ok((endpoint, location, admitted))
}

/// An exact transport endpoint admitted by the configured deployment. Raw
/// observation cannot construct a complete-cell producer capability.
#[derive(Debug)]
pub struct AdmittedCompilerEndpoint {
    endpoint: tidepool_extract_cmd::CompilerEndpoint,
    deployment: AdmittedCompilerDeployment,
}

impl AdmittedCompilerEndpoint {
    pub fn from_bound(
        endpoint: tidepool_extract_cmd::CompilerEndpoint,
    ) -> Result<Self, ToolchainError> {
        let deployment = admit_bound_endpoint(&endpoint)?;
        Ok(Self {
            endpoint,
            deployment,
        })
    }
    pub fn identity(&self) -> &tidepool_extract_cmd::CompilerIdentity {
        self.endpoint.identity()
    }
    pub fn deployment(&self) -> &AdmittedCompilerDeployment {
        &self.deployment
    }
    pub fn execute(
        self,
        command: &tidepool_extract_cmd::ExtractCmd,
    ) -> Result<tidepool_extract_cmd::ExtractRun, tidepool_extract_cmd::SpawnError> {
        self.endpoint.execute(command)
    }
    pub fn transaction(
        self,
    ) -> Result<tidepool_extract_cmd::CompilerTransaction, tidepool_extract_cmd::SpawnError> {
        self.endpoint.transaction()
    }
}

/// Admit an already-bound exact endpoint at the toolchain's compile entry
/// points. Observation never supplies missing configured deployment evidence.
pub fn admit_bound_endpoint(
    endpoint: &tidepool_extract_cmd::CompilerEndpoint,
) -> Result<AdmittedCompilerDeployment, ToolchainError> {
    CompilerDeploymentConfiguration::from_env()?
        .admit(
            *endpoint.identity().producer_bytes(),
            *endpoint.identity().consumed_worker_bytes(),
        )
        .map_err(Into::into)
}

// ---------------------------------------------------------------------------
// Stdlib location
// ---------------------------------------------------------------------------

/// A directory is a stdlib root iff `Tidepool/Prelude.hs` sits under it — the
/// same probe every historical policy used, now stated once.
#[must_use]
pub fn is_stdlib_root(dir: &Path) -> bool {
    dir.join("Tidepool").join("Prelude.hs").is_file()
}

/// A resolved stdlib root.
#[derive(Debug, Clone)]
pub struct StdlibLocation {
    /// The include dir to hand GHC (`Tidepool/Prelude.hs` lives under it).
    pub dir: PathBuf,
}

/// Binary-supplied tail steps of the stdlib precedence (steps 4 and 5). A
/// library caller with neither — e.g. session-decl validation inside
/// `tidepool-runtime` — passes [`StdlibFallbacks::default`] and gets steps 1–3.
#[derive(Debug, Default, Clone)]
pub struct StdlibFallbacks {
    /// Step 4: a materialized copy of the stdlib embedded in this binary.
    /// Materialize eagerly (it is sentinel-guarded and idempotent) and pass the
    /// directory; `None` for a binary that embeds no stdlib.
    pub bundle: Option<PathBuf>,
    /// Step 5: the source tree this binary was built from, typically
    /// `Path::new(env!("CARGO_MANIFEST_DIR")).parent()/bridge/haskell/lib`.
    pub build_tree: Option<PathBuf>,
}

/// Walk `start` and each of its ancestors (git-style), testing
/// `dir.join(candidate)` against `is_root` for each `candidate` in order.
/// Returns the first joined path that passes, or `None` if no ancestor has
/// one. Independent of which directory the caller happened to launch from —
/// a process started deep inside a checkout finds the same root as one
/// started at its top.
///
/// The one walk-up primitive shared by every in-repo tree locator (the
/// stdlib root here, the actors root in `bridge/facade`); do not hand-copy
/// this loop for a new candidate tree.
pub fn walk_up_for(
    start: &Path,
    candidates: &[&Path],
    is_root: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let mut cur = Some(start);
    while let Some(dir) = cur {
        for candidate in candidates {
            let joined = dir.join(candidate);
            if is_root(&joined) {
                return Some(joined);
            }
        }
        cur = dir.parent();
    }
    None
}

/// Resolve the Haskell stdlib root by the precedence table in the module docs.
///
/// # Errors
/// - [`ToolchainError::PreludeDirInvalid`] when `$TIDEPOOL_PRELUDE_DIR` is set
///   but is not a stdlib root (a bad override never falls through silently).
/// - [`ToolchainError::StdlibNotFound`] when no step found one; the error names
///   every path tried.
pub fn locate_stdlib(fallbacks: &StdlibFallbacks) -> Result<StdlibLocation, ToolchainError> {
    let mut tried: Vec<(&'static str, PathBuf)> = Vec::new();

    // 1. Operator override — authoritative, and loud when wrong.
    if let Some(dir) = std::env::var_os(ENV_PRELUDE_DIR) {
        let dir = PathBuf::from(dir);
        if is_stdlib_root(&dir) {
            return Ok(StdlibLocation { dir });
        }
        return Err(ToolchainError::PreludeDirInvalid { dir });
    }

    // 2. In-repo development: walk up from CWD, git-style. Walking (rather than
    //    probing CWD alone) is what makes this independent of which directory
    //    the native runner/the MCP client happened to launch from — a test running
    //    with CWD=<repo>/tidepool-runtime finds the same stdlib as a server
    //    launched from the repo root.
    if let Ok(cwd) = std::env::current_dir() {
        // `bridge/haskell/lib` from the checkout root or above, `haskell/lib`
        // from inside `bridge/`, `lib` from inside `bridge/haskell/`.
        let candidates = [
            Path::new("bridge/haskell/lib"),
            Path::new("haskell/lib"),
            Path::new("lib"),
        ];
        if let Some(found) = walk_up_for(&cwd, &candidates, is_stdlib_root) {
            return Ok(StdlibLocation { dir: found });
        }
        tried.push((
            "repo tree above cwd",
            cwd.join("bridge").join("haskell").join("lib"),
        ));
    }

    // 3. The `lib/` sibling of the extract's `dist-newstyle` (absorbs the old
    //    `derive_stdlib_include`).
    if let Some(candidate) = extract_sibling_lib() {
        if is_stdlib_root(&candidate) {
            return Ok(StdlibLocation { dir: candidate });
        }
        tried.push(("extract dist-newstyle sibling", candidate));
    }

    // 4/5. Binary-supplied fallbacks.
    for (what, candidate) in [
        ("bundled stdlib", &fallbacks.bundle),
        ("build tree", &fallbacks.build_tree),
    ] {
        let Some(candidate) = candidate else { continue };
        if is_stdlib_root(candidate) {
            return Ok(StdlibLocation {
                dir: candidate.clone(),
            });
        }
        tried.push((what, candidate.clone()));
    }

    Err(ToolchainError::StdlibNotFound { tried })
}

/// Walk `$TIDEPOOL_EXTRACT` upward to a `dist-newstyle` component and return
/// its sibling `lib/`. `None` when the override is unset or the binary does not
/// live inside a cabal build tree (the installed/nix case).
fn extract_sibling_lib() -> Option<PathBuf> {
    let extract = std::env::var_os(ENV_EXTRACT)?;
    let mut path = PathBuf::from(extract);
    if path.as_os_str().is_empty() {
        return None;
    }
    loop {
        if path.file_name().and_then(|n| n.to_str()) == Some("dist-newstyle") {
            return path.parent().map(|parent| parent.join("lib"));
        }
        if !path.pop() {
            return None;
        }
    }
}

/// Content fingerprint of the complete shipped stdlib source tree.
///
/// The canonical source traversal includes every namespace and production
/// Internal module, selects `.hs` files, and excludes generated Prelude_cbor
/// directories. Paths are relative to `dir`, so deployed bundles and authored
/// trees with the same shipped bytes compare identically.
///
/// # Errors
/// Refuses an identity if any selected source or directory cannot be read.
pub fn stdlib_fingerprint(dir: &Path) -> Result<String, crate::cache::SourceManifestError> {
    let manifest = crate::cache::shipped_haskell_source_manifest(dir)?;
    Ok(crate::cache::source_manifests_identity(
        b"tidepool-shipped-stdlib-v3",
        &[manifest],
    ))
}

// ---------------------------------------------------------------------------
// The deploy stamp + handshake
// ---------------------------------------------------------------------------

/// Stamp schema including the complete shipped-source fingerprint. A stamp
/// from another schema must be explicitly regenerated through deployment.
pub const STAMP_SCHEMA: u32 = 3;

/// The (extract, stdlib) pair that was last deployed together.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolchainStamp {
    /// [`STAMP_SCHEMA`] at write time.
    pub schema: u32,
    /// Producer identity reported by the bound compiler endpoint.
    pub extract: String,
    /// [`stdlib_fingerprint`] of the deployed stdlib tree.
    pub stdlib: String,
    /// Informational: where the extract was when the stamp was written.
    pub extract_path: String,
    /// Informational: where the stdlib was when the stamp was written.
    pub stdlib_path: String,
    /// Informational: what wrote it (binary name + version).
    pub written_by: String,
}

/// Where the deploy stamp lives: `$TIDEPOOL_TOOLCHAIN_STAMP`, else
/// `<cache_dir>/toolchain-stamp.json`.
///
/// It sits in the cache root deliberately: `scripts/redeploy.sh` rewrites it,
/// and a hand-cleared cache degrades to "no stamp" (a warning) rather than to
/// a stale stamp (a false alarm).
#[must_use]
pub fn stamp_path() -> PathBuf {
    if let Some(p) = std::env::var_os(ENV_STAMP) {
        return PathBuf::from(p);
    }
    crate::paths::cache_dir().join("toolchain-stamp.json")
}

/// Read the stamp, returning `Ok(None)` only when the file is absent.
///
/// # Errors
/// [`ToolchainError::Stamp`] if the file cannot be read or decoded, or its
/// schema requires regeneration through `scripts/redeploy.sh`.
pub fn read_stamp(path: &Path) -> Result<Option<ToolchainStamp>, ToolchainError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(ToolchainError::Stamp {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    match serde_json::from_str::<ToolchainStamp>(&text) {
        Ok(s) if s.schema == STAMP_SCHEMA => Ok(Some(s)),
        Ok(stamp) => Err(ToolchainError::Stamp {
            path: path.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "stamp schema {} requires schema {STAMP_SCHEMA}; run {REDEPLOY}",
                    stamp.schema
                ),
            ),
        }),
        Err(source) => Err(ToolchainError::Stamp {
            path: path.to_path_buf(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidData, source),
        }),
    }
}

/// Record the bound producer identity plus `stdlib` fingerprint in [`stamp_path`].
/// Called by `scripts/redeploy.sh` via `tidepool --write-toolchain-stamp`, so
/// the writer and the checker share one implementation and cannot drift.
///
/// Written via the shared durable atomic-write helper: the content lands in
/// a uniquely-named temp file in the stamp's own directory (so the rename
/// stays on one filesystem), fsynced, then renamed over the live stamp in
/// one syscall — a reader (this process's own next startup, or a concurrent
/// one) can never observe a short/partial write. The directory entry is
/// synced afterward so the rename itself, not just the temp file's bytes,
/// survives a crash.
///
/// # Errors
/// [`ToolchainError::Stamp`] if the stamp cannot be created or written, or
/// [`ToolchainError::StdlibManifest`] if the source tree is incomplete.
pub fn write_stamp(
    endpoint: &tidepool_extract_cmd::CompilerEndpoint,
    extract_path: &Path,
    stdlib: &Path,
) -> Result<ToolchainStamp, ToolchainError> {
    write_stamp_identity(endpoint.identity().producer_hex(), extract_path, stdlib)
}

fn write_stamp_identity(
    producer_identity: String,
    extract_path: &Path,
    stdlib: &Path,
) -> Result<ToolchainStamp, ToolchainError> {
    let stamp = ToolchainStamp {
        schema: STAMP_SCHEMA,
        extract: producer_identity,
        stdlib: stdlib_fingerprint(stdlib)?,
        extract_path: extract_path.display().to_string(),
        stdlib_path: stdlib.display().to_string(),
        written_by: format!("tidepool {}", env!("CARGO_PKG_VERSION")),
    };
    let path = stamp_path();
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|source| ToolchainError::Stamp {
        path: parent.to_path_buf(),
        source,
    })?;
    let json = serde_json::to_string_pretty(&stamp).map_err(|e| ToolchainError::Stamp {
        path: path.clone(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidData, e),
    })?;
    tidepool_atomic_write::write_durable(&path, json.as_bytes()).map_err(|e| {
        ToolchainError::Stamp {
            path: e.path,
            source: e.source,
        }
    })?;
    Ok(stamp)
}

/// Which side of the pair moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkewSide {
    /// The extract binary differs from the deployed one.
    Extract,
    /// The stdlib tree differs from the deployed one.
    Stdlib,
}

/// A detected extract/stdlib skew, rendered as the operator-facing message.
#[derive(Debug, Clone)]
pub struct SkewReport {
    /// Which side(s) moved.
    pub sides: Vec<SkewSide>,
    /// The extract path in use now.
    pub extract_path: PathBuf,
    /// The stdlib path in use now.
    pub stdlib_path: PathBuf,
    /// The stamp that was compared against.
    pub stamp: ToolchainStamp,
    /// Fingerprint of the extract in use now.
    pub extract_now: String,
    /// Fingerprint of the stdlib in use now.
    pub stdlib_now: String,
}

impl std::fmt::Display for SkewReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "toolchain skew: the extract binary and the Haskell stdlib in use were not deployed together."
        )?;
        for side in &self.sides {
            match side {
                SkewSide::Extract => writeln!(
                    f,
                    "  extract CHANGED: {} (now {}, deployed {})",
                    self.extract_path.display(),
                    short(&self.extract_now),
                    short(&self.stamp.extract),
                )?,
                SkewSide::Stdlib => writeln!(
                    f,
                    "  stdlib  CHANGED: {} (now {}, deployed {})",
                    self.stdlib_path.display(),
                    short(&self.stdlib_now),
                    short(&self.stamp.stdlib),
                )?,
            }
        }
        write!(
            f,
            "Running this pair produces wrong answers at eval time (unresolved imports, \
             malformed extract metadata) rather than a clean error. Fix it with `{REDEPLOY}`, \
             which moves the extract, both servers, and the stdlib together and rewrites the \
             stamp. To run a deliberately mixed pair (e.g. testing a worktree extract), set \
             {ENV_HANDSHAKE}=warn."
        )
    }
}

fn short(hex: &str) -> &str {
    &hex[..hex.len().min(12)]
}

/// What [`check_handshake`] found.
#[derive(Debug, Clone)]
pub enum HandshakeOutcome {
    /// Fingerprints match the stamp.
    Match,
    /// No usable stamp — nothing was ever deployed through `scripts/redeploy.sh`
    /// on this machine (or the cache was cleared since). Informational only.
    NoStamp {
        /// Where the stamp was looked for.
        path: PathBuf,
    },
    /// The pair in use is not the pair that was deployed.
    Skew(Box<SkewReport>),
}

/// Compare the located toolchain against the deploy stamp. Pure detection —
/// [`enforce_handshake`] applies the severity policy.
///
/// # Errors
/// [`ToolchainError::Stamp`] if the stamp is unreadable, corrupt, or requires
/// migration; [`ToolchainError::StdlibManifest`] if source inspection fails.
pub fn check_handshake(
    endpoint: &tidepool_extract_cmd::CompilerEndpoint,
    extract_path: &Path,
    stdlib: &Path,
) -> Result<HandshakeOutcome, ToolchainError> {
    check_handshake_identity(endpoint.identity().producer_hex(), extract_path, stdlib)
}

fn check_handshake_identity(
    extract_now: String,
    extract_path: &Path,
    stdlib: &Path,
) -> Result<HandshakeOutcome, ToolchainError> {
    let path = stamp_path();
    let Some(stamp) = read_stamp(&path)? else {
        return Ok(HandshakeOutcome::NoStamp { path });
    };

    let stdlib_now = stdlib_fingerprint(stdlib)?;
    let mut sides = Vec::new();
    if extract_now != stamp.extract {
        sides.push(SkewSide::Extract);
    }
    if stdlib_now != stamp.stdlib {
        sides.push(SkewSide::Stdlib);
    }
    if sides.is_empty() {
        return Ok(HandshakeOutcome::Match);
    }
    Ok(HandshakeOutcome::Skew(Box::new(SkewReport {
        sides,
        extract_path: extract_path.to_path_buf(),
        stdlib_path: stdlib.to_path_buf(),
        stamp,
        extract_now,
        stdlib_now,
    })))
}

/// Handshake severity, from `$TIDEPOOL_TOOLCHAIN_HANDSHAKE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandshakeSeverity {
    /// Default: a skew aborts startup.
    Error,
    /// Log the skew and continue — the escape hatch for deliberately testing a
    /// worktree extract against an installed server.
    Warn,
    /// Skip the check entirely (also skips the fingerprint work).
    Off,
}

impl HandshakeSeverity {
    /// Read `$TIDEPOOL_TOOLCHAIN_HANDSHAKE`. An unrecognized value is `Error`:
    /// a typo must not silently disable the check.
    #[must_use]
    pub fn from_env() -> Self {
        match std::env::var(ENV_HANDSHAKE).unwrap_or_default().as_str() {
            "warn" => Self::Warn,
            "off" => Self::Off,
            _ => Self::Error,
        }
    }
}

/// Run the startup handshake and apply the severity policy. Call once, at
/// server startup, after the toolchain is located — never per-eval.
///
/// Returns the outcome so the caller can log non-fatal cases. Under the explicit
/// [`HandshakeSeverity::Warn`] override, stamp or source-manifest verification
/// errors are logged and returned as [`HandshakeOutcome::NoStamp`].
///
/// # Errors
/// [`ToolchainError::Skew`] when a skew is detected and severity is
/// [`HandshakeSeverity::Error`]. Under that default policy, unreadable, corrupt
/// or obsolete stamps return [`ToolchainError::Stamp`], and incomplete source
/// inspection returns [`ToolchainError::StdlibManifest`].
pub fn enforce_handshake(
    endpoint: &tidepool_extract_cmd::CompilerEndpoint,
    extract_path: &Path,
    stdlib: &Path,
) -> Result<HandshakeOutcome, ToolchainError> {
    enforce_handshake_identity(endpoint.identity().producer_hex(), extract_path, stdlib)
}

fn enforce_handshake_identity(
    producer_identity: String,
    extract_path: &Path,
    stdlib: &Path,
) -> Result<HandshakeOutcome, ToolchainError> {
    let severity = HandshakeSeverity::from_env();
    if severity == HandshakeSeverity::Off {
        return Ok(HandshakeOutcome::NoStamp { path: stamp_path() });
    }
    let outcome = match check_handshake_identity(producer_identity, extract_path, stdlib) {
        Ok(outcome) => outcome,
        Err(e) if severity == HandshakeSeverity::Warn => {
            tracing::warn!("toolchain handshake could not be verified: {e}");
            HandshakeOutcome::NoStamp { path: stamp_path() }
        }
        Err(e) => return Err(e),
    };
    match (&outcome, severity) {
        (HandshakeOutcome::Skew(report), HandshakeSeverity::Error) => {
            Err(ToolchainError::Skew(report.clone()))
        }
        _ => Ok(outcome),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn configured_deployment() -> CompilerDeploymentConfiguration {
        CompilerDeploymentConfiguration::Configured(CompilerDeploymentAuthority {
            schema: 1,
            producer_identity: [3; 32],
            consumed_worker_identity: [4; 32],
            frontend_path: PathBuf::from("/configured/frontend"),
            worker_path: PathBuf::from("/configured/worker"),
            ghc_libdir: PathBuf::from("/configured/ghc/lib"),
        })
    }

    #[test]
    fn deployment_admission_rejects_unknown_authority() {
        assert_eq!(
            CompilerDeploymentConfiguration::Unknown.admit([3; 32], [4; 32]),
            Err(DeploymentAdmissionError::Unknown)
        );
    }

    #[test]
    fn deployment_admission_rejects_a_producer_outside_configured_deployment() {
        assert_eq!(
            configured_deployment().admit([8; 32], [4; 32]),
            Err(DeploymentAdmissionError::ProducerMismatch)
        );
    }

    #[test]
    fn deployment_admission_rejects_a_changed_consumed_worker() {
        assert_eq!(
            configured_deployment().admit([3; 32], [9; 32]),
            Err(DeploymentAdmissionError::WorkerMismatch)
        );
    }

    #[test]
    fn configured_admission_preserves_both_original_and_consumed_identity() {
        let accepted = configured_deployment().admit([3; 32], [4; 32]).unwrap();
        assert_eq!(accepted.producer_identity, [3; 32]);
        assert_eq!(accepted.consumed_worker_identity, [4; 32]);
    }

    #[test]
    fn extractor_roles_are_not_interchangeable() {
        assert_eq!(
            classify_extract_binary(b"Usage: tidepool-extract [OPTIONS] <file.hs> ...\n"),
            ExtractBinaryRole::Frontend
        );
        assert_eq!(
            classify_extract_binary(b"worker requires exactly one versioned request\n"),
            ExtractBinaryRole::Worker
        );
        assert_eq!(
            classify_extract_binary(b"Usage: tidepool-extract-bin [OPTIONS]\n"),
            ExtractBinaryRole::Unknown
        );
    }

    fn test_producer_identity(path: &Path) -> String {
        blake3::hash(&std::fs::read(path).unwrap())
            .to_hex()
            .to_string()
    }

    fn write_stamp_test(extract: &Path, stdlib: &Path) -> Result<ToolchainStamp, ToolchainError> {
        write_stamp_identity(test_producer_identity(extract), extract, stdlib)
    }

    fn check_handshake_test(
        extract: &Path,
        stdlib: &Path,
    ) -> Result<HandshakeOutcome, ToolchainError> {
        check_handshake_identity(test_producer_identity(extract), extract, stdlib)
    }

    fn enforce_handshake_test(
        extract: &Path,
        stdlib: &Path,
    ) -> Result<HandshakeOutcome, ToolchainError> {
        enforce_handshake_identity(test_producer_identity(extract), extract, stdlib)
    }

    /// A set-but-nonexistent `$TIDEPOOL_EXTRACT` must be a hard error out of
    /// [`extract_command_name`], naming the offending path — the exact
    /// silent-degrade-to-`$PATH` failure this module exists to close (a
    /// caller must never spawn a different binary than the override named).
    /// Unsetting the var is the other half of the contract: it must fall
    /// back to the bare `$PATH` name unchanged, not become an error too.
    #[test]
    #[serial]
    fn extract_command_name_is_strict_about_a_bad_override() {
        let original = std::env::var(ENV_EXTRACT).ok();

        let bad_path = "/nonexistent/tidepool-extract-loudfail-test";
        std::env::set_var(ENV_EXTRACT, bad_path);
        let err = extract_command_name().unwrap_err();
        assert!(
            matches!(err, ToolchainError::ExtractNotFound { .. }),
            "expected ExtractNotFound, got {err:?}"
        );
        let msg = err.to_string();
        assert!(msg.contains(bad_path), "message must name the path: {msg}");
        assert!(
            msg.contains("$PATH"),
            "message must say unsetting falls back to $PATH: {msg}"
        );

        std::env::remove_var(ENV_EXTRACT);
        assert_eq!(
            extract_command_name().unwrap().as_path(),
            Path::new(tidepool_extract_cmd::DEFAULT_BIN),
            "an unset override must still fall back to the bare $PATH name"
        );

        match original {
            Some(v) => std::env::set_var(ENV_EXTRACT, v),
            None => std::env::remove_var(ENV_EXTRACT),
        }
    }

    fn write_stdlib(root: &Path, prelude_body: &str) {
        let tp = root.join("Tidepool");
        std::fs::create_dir_all(tp.join("Internal")).unwrap();
        std::fs::write(tp.join("Prelude.hs"), prelude_body).unwrap();
        std::fs::write(tp.join("Table.hs"), "module Tidepool.Table where\n").unwrap();
        // Production Internal modules ship with every other namespace.
        std::fs::write(
            tp.join("Internal").join("ExitCell.hs"),
            "module Tidepool.Internal.ExitCell where\n",
        )
        .unwrap();
        std::fs::write(tp.join("notes.txt"), "not haskell\n").unwrap();
        std::fs::create_dir_all(root.join("Jev")).unwrap();
        std::fs::write(
            root.join("Jev/Operators.hs"),
            "module Jev.Operators where\n",
        )
        .unwrap();
    }

    /// The fingerprint is content-addressed, not path-addressed: the same tree
    /// materialized at two paths (repo vs bundle) must compare equal, or every
    /// deployed server would report skew against its own bundle.
    #[test]
    fn stdlib_fingerprint_is_path_independent() {
        let a = tempfile::TempDir::new().unwrap();
        let b = tempfile::TempDir::new().unwrap();
        write_stdlib(a.path(), "module Tidepool.Prelude where\n");
        write_stdlib(b.path(), "module Tidepool.Prelude where\n");
        assert_eq!(
            stdlib_fingerprint(a.path()).unwrap(),
            stdlib_fingerprint(b.path()).unwrap(),
            "same content at different paths must fingerprint identically"
        );
    }

    #[test]
    fn stdlib_fingerprint_tracks_all_shipped_namespaces_and_ignores_generated_artifacts() {
        let dir = tempfile::TempDir::new().unwrap();
        write_stdlib(dir.path(), "module Tidepool.Prelude where\n");
        let initial = stdlib_fingerprint(dir.path()).unwrap();
        std::fs::write(
            dir.path().join("Tidepool/Internal/ExitCell.hs"),
            "module Tidepool.Internal.ExitCell where\nvalue = ()\n",
        )
        .unwrap();
        let internal = stdlib_fingerprint(dir.path()).unwrap();
        assert_ne!(
            initial, internal,
            "production Internal modules must be covered"
        );
        std::fs::write(
            dir.path().join("Jev/Operators.hs"),
            "module Jev.Operators where\nvalue = ()\n",
        )
        .unwrap();
        let namespaces = stdlib_fingerprint(dir.path()).unwrap();
        assert_ne!(
            internal, namespaces,
            "every shipped namespace must be covered"
        );

        let generated = dir.path().join("Tidepool/Prelude_cbor");
        std::fs::create_dir_all(&generated).unwrap();
        std::fs::write(generated.join("Generated.hs"), "not shipped\n").unwrap();
        std::os::unix::fs::symlink("absent.hs", generated.join("Incomplete.hs")).unwrap();
        std::fs::write(dir.path().join("Tidepool/Prelude.hs-boot"), "not shipped\n").unwrap();
        std::fs::write(dir.path().join("Tidepool/notes.txt"), "changed notes\n").unwrap();
        assert_eq!(namespaces, stdlib_fingerprint(dir.path()).unwrap());
        std::fs::write(
            dir.path().join("Tidepool/Prelude.hs"),
            "module Tidepool.Prelude where\nnewThing = ()\n",
        )
        .unwrap();
        assert_ne!(namespaces, stdlib_fingerprint(dir.path()).unwrap());
    }

    #[test]
    fn stdlib_fingerprint_refuses_incomplete_source_evidence() {
        let dir = tempfile::TempDir::new().unwrap();
        let missing = dir.path().join("absent");
        assert_eq!(stdlib_fingerprint(&missing).unwrap_err().path, missing);
        write_stdlib(dir.path(), "module Tidepool.Prelude where\n");
        let broken = dir.path().join("Jev/Broken.hs");
        std::os::unix::fs::symlink("missing.hs", &broken).unwrap();
        assert_eq!(stdlib_fingerprint(dir.path()).unwrap_err().path, broken);
    }

    #[test]
    fn is_stdlib_root_probes_prelude() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(!is_stdlib_root(dir.path()));
        write_stdlib(dir.path(), "module Tidepool.Prelude where\n");
        assert!(is_stdlib_root(dir.path()));
    }

    /// A set-but-wrong `$TIDEPOOL_PRELUDE_DIR` must be a hard error, never a
    /// silent fall-through to some other stdlib — a typo'd override that
    /// quietly served a different tree is the exact failure this module exists
    /// to kill.
    #[test]
    fn prelude_dir_override_that_is_not_a_stdlib_root_is_fatal() {
        let dir = tempfile::TempDir::new().unwrap();
        std::env::set_var(ENV_PRELUDE_DIR, dir.path());
        let err = locate_stdlib(&StdlibFallbacks::default()).unwrap_err();
        std::env::remove_var(ENV_PRELUDE_DIR);
        assert!(
            matches!(err, ToolchainError::PreludeDirInvalid { .. }),
            "expected PreludeDirInvalid, got {err:?}"
        );
        assert!(err.to_string().contains("Tidepool/Prelude.hs"));
    }

    /// Isolate the machine-wide fingerprint sidecar + stamp so a test never
    /// reads or writes the developer's real cache.
    fn isolate_cache(tmp: &Path) {
        std::env::set_var("XDG_CACHE_HOME", tmp.join("cache"));
        std::env::set_var(ENV_STAMP, tmp.join("stamp.json"));
    }

    /// The handshake's whole job: a stamp recorded at deploy time, then ONE
    /// side of the pair moves, and startup says so loudly instead of serving a
    /// mixed toolchain that fails at eval time.
    #[test]
    #[serial]
    fn handshake_detects_a_skewed_extract_and_names_the_redeploy_script() {
        let tmp = tempfile::TempDir::new().unwrap();
        isolate_cache(tmp.path());
        let stdlib = tmp.path().join("lib");
        write_stdlib(&stdlib, "module Tidepool.Prelude where\n");
        let extract = tmp.path().join("tidepool-extract");
        std::fs::write(&extract, b"deployed extract v1").unwrap();

        // Deploy: both sides blessed together.
        write_stamp_test(&extract, &stdlib).unwrap();
        assert!(
            matches!(
                check_handshake_test(&extract, &stdlib).unwrap(),
                HandshakeOutcome::Match
            ),
            "the pair that was just stamped must match"
        );

        // A `nix profile upgrade tidepool-extract` without a full redeploy.
        std::fs::write(&extract, b"upgraded extract v2 -- larger").unwrap();

        let outcome = check_handshake_test(&extract, &stdlib).unwrap();
        let HandshakeOutcome::Skew(report) = outcome else {
            panic!("expected skew, got {outcome:?}");
        };
        assert_eq!(report.sides, vec![SkewSide::Extract]);

        // Default severity aborts startup, and the message is actionable.
        std::env::remove_var(ENV_HANDSHAKE);
        let err = enforce_handshake_test(&extract, &stdlib).unwrap_err();
        let msg = err.to_string();
        assert!(matches!(err, ToolchainError::Skew(_)), "got {err:?}");
        assert!(msg.contains(REDEPLOY), "message must name the fix: {msg}");
        assert!(
            msg.contains(ENV_HANDSHAKE),
            "message must name the escape hatch: {msg}"
        );

        // `warn` is the documented escape hatch for a deliberately mixed pair.
        std::env::set_var(ENV_HANDSHAKE, "warn");
        assert!(
            matches!(
                enforce_handshake_test(&extract, &stdlib).unwrap(),
                HandshakeOutcome::Skew(_)
            ),
            "warn severity reports the skew but does not fail"
        );
        std::env::remove_var(ENV_HANDSHAKE);
    }

    /// A stdlib edit with an unchanged extract is the other half of the pair,
    /// and must be caught the same way — this is the "edited bridge/haskell/lib, never
    /// redeployed" case.
    #[test]
    #[serial]
    fn handshake_detects_a_skewed_stdlib() {
        let tmp = tempfile::TempDir::new().unwrap();
        isolate_cache(tmp.path());
        let stdlib = tmp.path().join("lib");
        write_stdlib(&stdlib, "module Tidepool.Prelude where\n");
        let extract = tmp.path().join("tidepool-extract");
        std::fs::write(&extract, b"deployed extract v1").unwrap();
        write_stamp_test(&extract, &stdlib).unwrap();

        std::fs::write(
            stdlib.join("Tidepool").join("Prelude.hs"),
            "module Tidepool.Prelude where\nadded = ()\n",
        )
        .unwrap();

        let HandshakeOutcome::Skew(report) = check_handshake_test(&extract, &stdlib).unwrap()
        else {
            panic!("a stdlib edit must skew");
        };
        assert_eq!(report.sides, vec![SkewSide::Stdlib]);
    }

    /// No stamp means nobody has deployed through `scripts/redeploy.sh` on this
    /// machine (or the cache was hand-cleared). That is informational — a fresh
    /// checkout must not be unable to start.
    #[test]
    #[serial]
    fn handshake_without_a_stamp_is_informational() {
        let tmp = tempfile::TempDir::new().unwrap();
        isolate_cache(tmp.path());
        let stdlib = tmp.path().join("lib");
        write_stdlib(&stdlib, "module Tidepool.Prelude where\n");
        let extract = tmp.path().join("tidepool-extract");
        std::fs::write(&extract, b"extract").unwrap();
        std::env::remove_var(ENV_HANDSHAKE);

        assert!(matches!(
            enforce_handshake_test(&extract, &stdlib).unwrap(),
            HandshakeOutcome::NoStamp { .. }
        ));
    }

    /// Different fingerprint algorithms require an explicit deployment migration.
    #[test]
    #[serial]
    fn stamp_with_a_foreign_schema_requires_regeneration() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("stamp.json");
        for schema in [2, STAMP_SCHEMA + 1] {
            std::fs::write(
                &path,
                serde_json::json!({
                    "schema": schema,
                    "extract": "aa", "stdlib": "bb",
                    "extract_path": "/x", "stdlib_path": "/y", "written_by": "other schema",
                })
                .to_string(),
            )
            .unwrap();
            let error = read_stamp(&path).unwrap_err().to_string();
            assert!(error.contains("schema"), "{error}");
            assert!(error.contains(REDEPLOY), "{error}");
        }
    }

    #[test]
    fn unreadable_stamp_is_refused_and_missing_stamp_is_absent() {
        let directory = tempfile::TempDir::new().unwrap();
        assert!(read_stamp(directory.path()).is_err());
        assert!(read_stamp(&directory.path().join("absent.json"))
            .unwrap()
            .is_none());
    }

    #[test]
    #[serial]
    fn incomplete_stdlib_cannot_write_a_deployment_stamp() {
        let tmp = tempfile::TempDir::new().unwrap();
        isolate_cache(tmp.path());
        let extract = tmp.path().join("extract");
        std::fs::write(&extract, "producer").unwrap();
        let error = write_stamp_test(&extract, &tmp.path().join("missing-stdlib")).unwrap_err();
        assert!(matches!(error, ToolchainError::StdlibManifest(_)));
        assert!(!stamp_path().exists());
    }

    /// Simulates a crash between `NamedTempFile` creation and `persist`'s
    /// rename: a leftover temp file sits next to a real, already-written
    /// stamp. The real stamp must read back intact (the leftover is a
    /// different path entirely — atomic rename means a torn write can never
    /// land ON the stamp's own path), and a later, ordinary write must still
    /// succeed (its own uniquely-named temp file never collides with the
    /// leftover).
    #[test]
    #[serial]
    fn write_stamp_is_atomic_against_an_interrupted_write() {
        let tmp = tempfile::TempDir::new().unwrap();
        isolate_cache(tmp.path());
        let stdlib = tmp.path().join("lib");
        write_stdlib(&stdlib, "module Tidepool.Prelude where\n");
        let extract = tmp.path().join("tidepool-extract");
        std::fs::write(&extract, b"deployed extract v1").unwrap();

        let deployed = write_stamp_test(&extract, &stdlib).unwrap();

        let path = stamp_path();
        let leftover = path.parent().unwrap().join(".toolchain-stamp.tmp-leftover");
        std::fs::write(&leftover, b"{ garbage, not json, never persisted").unwrap();

        let read_back = read_stamp(&path).unwrap().expect("stamp intact");
        assert_eq!(read_back.extract, deployed.extract);
        assert_eq!(read_back.stdlib, deployed.stdlib);

        // A fresh write is unaffected by the stray leftover and still lands
        // cleanly.
        write_stamp_test(&extract, &stdlib).unwrap();
        assert!(
            read_stamp(&path).unwrap().is_some(),
            "a later write must still succeed with a leftover temp file present"
        );
        assert!(
            leftover.exists(),
            "a fresh write must not touch an unrelated leftover file"
        );
    }

    /// The stamp protocol's other half: content-level corruption (not a
    /// partial write, but e.g. a hand-edited or truncated-and-then-appended
    /// file) must fail closed under the default `Error` severity, and be
    /// reported — not silently swallowed as "no stamp" — under `Warn`.
    #[test]
    #[serial]
    fn corrupt_stamp_fails_closed_in_error_and_warns_in_warn() {
        let tmp = tempfile::TempDir::new().unwrap();
        isolate_cache(tmp.path());
        let stdlib = tmp.path().join("lib");
        write_stdlib(&stdlib, "module Tidepool.Prelude where\n");
        let extract = tmp.path().join("tidepool-extract");
        std::fs::write(&extract, b"deployed extract v1").unwrap();

        let path = stamp_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{ not valid json at all").unwrap();

        // `read_stamp` itself distinguishes corrupt from absent.
        let err = read_stamp(&path).unwrap_err();
        assert!(matches!(err, ToolchainError::Stamp { .. }), "got {err:?}");

        // Default severity fails closed rather than proceeding as if
        // nothing were deployed.
        std::env::remove_var(ENV_HANDSHAKE);
        let err = enforce_handshake_test(&extract, &stdlib).unwrap_err();
        assert!(matches!(err, ToolchainError::Stamp { .. }), "got {err:?}");

        // `warn` degrades gracefully (logs and continues) instead of
        // aborting — observably, it must not error, and must not be
        // mistaken for a genuine deploy (it degrades to the same
        // no-stamp-detected outcome `check_handshake` would report for an
        // absent file).
        std::env::set_var(ENV_HANDSHAKE, "warn");
        let outcome = enforce_handshake_test(&extract, &stdlib).unwrap();
        assert!(
            matches!(outcome, HandshakeOutcome::NoStamp { .. }),
            "got {outcome:?}"
        );
        std::env::remove_var(ENV_HANDSHAKE);
    }
}
