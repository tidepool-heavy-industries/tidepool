//! The single eval failure-classification surface, shared by both servers.
//!
//! An eval failure has two orthogonal axes a caller routes on:
//!
//! - **class** — WHAT failed, and therefore what to do about it:
//!   [`FailureClass::UserHaskell`] (GHC rejected the code → rewrite it),
//!   [`FailureClass::Runtime`] (the JIT/eval engine failed at run time, incl.
//!   aborts and resource exhaustion → a runtime problem, not a source edit),
//!   [`FailureClass::Infra`] (the extractor could not spawn, or a cache/IO
//!   operation failed → an environment problem), and
//!   [`FailureClass::VersionSkew`] (the extractor's wire format was rejected by
//!   this server's reader → redeploy/reconnect so both sides run one build).
//! - **phase** — WHEN it failed: [`Phase::Compile`] (during source compilation
//!   extraction), [`Phase::Install`] (while parsing, linking, compiling, or
//!   admitting a prepared program before execution), or [`Phase::Run`] (during
//!   JIT execution).
//!
//! Splitting the two axes is the fix for the class of bug where a wire-format
//! version skew (new extractor, old server) surfaced tagged as a user Haskell
//! error, so a caller branching on the class misrouted to "rewrite your code"
//! instead of "redeploy the extractor".
//!
//! [`classify_compile`] is THE classifier — the one decision tree mapping an
//! error to its `(class, phase)`. `tidepool_runtime::failclass::classify` (a
//! two-arm dispatch over the unified `RuntimeError`) and `::classify_session`
//! (over `SessionError`) both reuse it — they stay in `tidepool-runtime`
//! rather than here because `RuntimeError`/`SessionError` are session/JIT
//! types this crate sits below and must not depend on.

use crate::CompileError;
use tidepool_repr::serial::{ReadError, VERSION_MAJOR, VERSION_MINOR};

/// WHAT failed — the axis a caller routes on. Stable lowercase tags via
/// [`FailureClass::tag`]; never reword them, they are the machine-greppable
/// vocabulary a caller branches on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FailureClass {
    /// GHC parse/type/scope error in the user's code, or an unsupported IO
    /// result type — the extractor ran and rejected the source. Fix the code.
    UserHaskell,
    /// An immutable compiler input/authority contract was rejected before compilation.
    InputRejected,
    /// A JIT/eval run-time failure: a Haskell `error`/`undefined`, an unhandled
    /// effect, resource exhaustion (stack/heap overflow, blackhole), a caught
    /// JIT signal/trap, a thread-killing crash, an abort, or a run-phase
    /// timeout. The engine failed while running — not a source edit.
    Runtime,
    /// The extractor could not be spawned, produced no output, or a cache/IO
    /// operation failed — an environment problem, not the user's code.
    Infra,
    /// The extractor emitted a wire format this server's reader rejected (a
    /// missing/old header, an unsupported version, or a shape mismatch), whether
    /// read from fresh extract output or a cache. The two sides are built
    /// against different formats: redeploy/reconnect.
    VersionSkew,
}

impl FailureClass {
    /// Stable lowercase tag, safe to grep and count.
    #[must_use]
    pub fn tag(self) -> &'static str {
        match self {
            FailureClass::UserHaskell => "user-haskell",
            FailureClass::InputRejected => "input-rejected",
            FailureClass::Runtime => "runtime",
            FailureClass::Infra => "infra",
            FailureClass::VersionSkew => "version-skew",
        }
    }
}

/// WHEN it failed — source extraction, prepared-program installation, or
/// execution. The `install` stage distinguishes failures before a prepared
/// program starts running from failures produced by execution itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    /// During source compilation (the `tidepool-extract` shell-out + read).
    Compile,
    /// While parsing, linking, compiling, or admitting a prepared program,
    /// before its machine executes.
    Install,
    /// During JIT execution of the compiled program.
    Run,
}

impl Phase {
    /// Stable lowercase tag, safe to grep and count.
    #[must_use]
    pub fn tag(self) -> &'static str {
        match self {
            Phase::Compile => "compile",
            Phase::Install => "install",
            Phase::Run => "run",
        }
    }
}

/// A classified failure: the two routing axes plus the human-facing message.
/// Successful evals never build one — this rides only the error path.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct FailureEnvelope {
    /// WHAT failed.
    pub class: FailureClass,
    /// WHEN it failed.
    pub phase: Phase,
    /// Human-facing message (already re-messaged for a version skew; the
    /// error's own `Display` text otherwise).
    #[serde(skip_serializing)]
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cause: Option<CompileFailureCause>,
}

/// Compiler-owned diagnostic category; it grants no compilation authority.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CompileFailureCause {
    Io,
    ExtractorContract,
    InputRejected,
    SourceDiagnostics,
    WorkerFailure,
    MalformedDiagnostics,
    WireDecode,
    PreparedArtifact,
    MissingOutput,
    TypedSites,
    UnsupportedIo,
    ConstructorIdentity,
    CompilerEvidence {
        failure: CompilerEvidenceFailure,
    },
    ArtifactInventory {
        failure: crate::artifact_inventory::ArtifactInventoryFailure,
    },
    CompileInput {
        failure: crate::CompileInputError,
    },
    ModulePackage {
        failure: ModulePackageFailure,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CompilerEvidenceFailure {
    SourceEvidence {
        input: std::path::PathBuf,
        failure: crate::cache::DependencyEvidenceFailure,
    },
    Read {
        path: std::path::PathBuf,
        failure: EvidenceReadCause,
    },
    ByteLimit {
        owner: EvidenceByteLimitOwner,
        actual: usize,
        limit: usize,
    },
    Budget {
        resource: String,
    },
    Contract,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceByteLimitOwner {
    Inventory,
    Module,
    Decode,
    Certificate,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EvidenceReadCause {
    NonAbsolutePath,
    NotFile,
    SizeLimit {
        actual: u64,
        limit: u64,
    },
    Io {
        operation: crate::certified_products::EvidenceReadOperation,
    },
    LengthChanged {
        expected: u64,
        actual: u64,
    },
}

impl From<&crate::certified_products::CertificationError> for CompilerEvidenceFailure {
    fn from(error: &crate::certified_products::CertificationError) -> Self {
        use crate::certified_products::{CertificationError, EvidenceReadFailure};
        use crate::recovery_artifacts::{RecoveryAdmissionFailure, RecoveryArtifactError};
        use tidepool_repr::execution_schema::ParseError;
        match error {
            CertificationError::CapturedModulePayload(
                RecoveryArtifactError::CompletedSourceEvidence { input, failure },
            ) => Self::SourceEvidence {
                input: input.clone(),
                failure: (**failure).clone(),
            },
            CertificationError::CompletedSourceEvidence { input, failure } => {
                Self::SourceEvidence {
                    input: input.clone(),
                    failure: (**failure).clone(),
                }
            }
            CertificationError::EvidenceRead { path, failure } => Self::Read {
                path: path.clone(),
                failure: match failure {
                    EvidenceReadFailure::NonAbsolutePath => EvidenceReadCause::NonAbsolutePath,
                    EvidenceReadFailure::NotFile => EvidenceReadCause::NotFile,
                    EvidenceReadFailure::SizeLimit { actual, limit } => {
                        EvidenceReadCause::SizeLimit {
                            actual: *actual,
                            limit: *limit,
                        }
                    }
                    EvidenceReadFailure::Io { operation, .. } => EvidenceReadCause::Io {
                        operation: *operation,
                    },
                    EvidenceReadFailure::LengthChanged { expected, actual } => {
                        EvidenceReadCause::LengthChanged {
                            expected: *expected,
                            actual: *actual,
                        }
                    }
                },
            },
            CertificationError::SizeLimit { actual, limit, .. } => Self::ByteLimit {
                owner: EvidenceByteLimitOwner::Certificate,
                actual: *actual,
                limit: *limit,
            },
            CertificationError::Product(ParseError::InventoryByteLimit { actual, limit }) => {
                Self::ByteLimit {
                    owner: EvidenceByteLimitOwner::Inventory,
                    actual: *actual,
                    limit: *limit,
                }
            }
            CertificationError::Product(ParseError::ModuleByteLimit { actual, limit }) => {
                Self::ByteLimit {
                    owner: EvidenceByteLimitOwner::Module,
                    actual: *actual,
                    limit: *limit,
                }
            }
            CertificationError::Product(ParseError::ByteLimit { actual, limit }) => {
                Self::ByteLimit {
                    owner: EvidenceByteLimitOwner::Decode,
                    actual: *actual,
                    limit: *limit,
                }
            }
            CertificationError::Product(ParseError::LimitExceeded(resource)) => Self::Budget {
                resource: (*resource).to_owned(),
            },
            CertificationError::CapturedModulePayload(
                RecoveryArtifactError::InventoryAccounting(failure),
            ) => match failure {
                RecoveryAdmissionFailure::Decode(error) => {
                    Self::from(&CertificationError::Product(error.clone()))
                }
                RecoveryAdmissionFailure::CertificateSize { actual, limit, .. } => {
                    Self::ByteLimit {
                        owner: EvidenceByteLimitOwner::Certificate,
                        actual: *actual,
                        limit: *limit,
                    }
                }
                RecoveryAdmissionFailure::EvidenceSize {
                    path,
                    actual,
                    limit,
                } => Self::Read {
                    path: path.clone(),
                    failure: EvidenceReadCause::SizeLimit {
                        actual: *actual,
                        limit: *limit,
                    },
                },
            },
            _ => Self::Contract,
        }
    }
}

/// Structured projection of the package owner's refusal. I/O details remain
/// in the human diagnostic; routing never depends on their rendered text.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModulePackageFailure {
    Io {
        path: std::path::PathBuf,
    },
    Format,
    Bounds,
    CompilerMismatch,
    UnknownCompiler,
    CompilerConfiguration,
    RootMoved,
    MutableRoot,
    SourceAlias {
        path: std::path::PathBuf,
    },
    SourceChanged,
    ArtifactChanged {
        path: std::path::PathBuf,
    },
    OpenCohort,
    IncompleteProduct {
        unit: String,
        module: String,
        availability: crate::cache::ProductAvailability,
    },
}

impl From<&crate::toolchain::ModulePackageError> for ModulePackageFailure {
    fn from(error: &crate::toolchain::ModulePackageError) -> Self {
        use crate::toolchain::ModulePackageError;
        match error {
            ModulePackageError::Io { path, .. } => Self::Io { path: path.clone() },
            ModulePackageError::Format(_) => Self::Format,
            ModulePackageError::Bounds => Self::Bounds,
            ModulePackageError::CompilerMismatch => Self::CompilerMismatch,
            ModulePackageError::UnknownCompiler => Self::UnknownCompiler,
            ModulePackageError::CompilerConfiguration(_) => Self::CompilerConfiguration,
            ModulePackageError::RootMoved => Self::RootMoved,
            ModulePackageError::MutableRoot => Self::MutableRoot,
            ModulePackageError::SourceAlias(path) => Self::SourceAlias { path: path.clone() },
            ModulePackageError::SourceChanged => Self::SourceChanged,
            ModulePackageError::ArtifactChanged(path) => {
                Self::ArtifactChanged { path: path.clone() }
            }
            ModulePackageError::OpenCohort => Self::OpenCohort,
            ModulePackageError::IncompleteProduct {
                unit,
                module,
                availability,
            } => Self::IncompleteProduct {
                unit: unit.clone(),
                module: module.clone(),
                availability: *availability,
            },
        }
    }
}

impl std::fmt::Display for FailureEnvelope {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(output)
    }
}

impl From<String> for FailureEnvelope {
    fn from(message: String) -> Self {
        Self::new(FailureClass::Infra, Phase::Compile, message)
    }
}

impl From<&str> for FailureEnvelope {
    fn from(message: &str) -> Self {
        Self::from(message.to_owned())
    }
}

impl FailureEnvelope {
    /// `pub` (not crate-private) because `tidepool_runtime::failclass::classify`/
    /// `::classify_session` construct envelopes from outside this crate — see
    /// this module's doc.
    pub fn new(class: FailureClass, phase: Phase, message: String) -> Self {
        Self {
            class,
            phase,
            message,
            cause: None,
        }
    }
}

/// THE classifier: map a [`CompileError`] to its `(class, phase)` and message.
///
/// A wire-format rejection ([`CompileError::ReadError`]) is re-messaged into a
/// self-diagnosing skew report (see `version_skew_message`); every other
/// variant keeps its own Display text.
#[must_use]
pub fn classify_compile(err: &CompileError) -> FailureEnvelope {
    let mut envelope = match err {
        CompileError::CompilerEvidence(error) => {
            let failure = CompilerEvidenceFailure::from(error.as_ref());
            let class = match failure {
                CompilerEvidenceFailure::Read {
                    failure: EvidenceReadCause::NonAbsolutePath,
                    ..
                }
                | CompilerEvidenceFailure::Contract => FailureClass::VersionSkew,
                _ => FailureClass::Infra,
            };
            FailureEnvelope::new(class, Phase::Compile, err.to_string())
        }
        // Real GHC rejections carry `Diagnostics`. This arm is reserved for a
        // malformed extractor artifact or impossible internal request shape.
        CompileError::ExtractFailed(_)
        | CompileError::ArtifactInventory(_)
        | CompileError::CompileInput(_)
        | CompileError::ModulePackage(_) => {
            FailureEnvelope::new(FailureClass::VersionSkew, Phase::Compile, err.to_string())
        }
        // The extractor ran, exited non-zero, and its stdout parsed as a valid
        // diagnostics report — a real GHC compile failure with real spans. The
        // message here is a plain-text fallback (no source/anchor context) for
        // callers that only ever see `env.message`; richer callers render the
        // structured diagnostics themselves via `crate::diag::render_diagnostics`.
        CompileError::Diagnostics(diags) => FailureEnvelope::new(
            FailureClass::UserHaskell,
            Phase::Compile,
            diags
                .iter()
                .map(|d| d.message.as_str())
                .collect::<Vec<_>>()
                .join("\n\n"),
        ),
        CompileError::InputRejected(diags) => FailureEnvelope::new(
            FailureClass::InputRejected,
            Phase::Compile,
            diags
                .iter()
                .map(|d| d.message.as_str())
                .collect::<Vec<_>>()
                .join("\n\n"),
        ),
        CompileError::WorkerFailure(diags) => FailureEnvelope::new(
            FailureClass::Infra,
            Phase::Compile,
            diags
                .iter()
                .map(|d| d.message.as_str())
                .collect::<Vec<_>>()
                .join("\n\n"),
        ),
        // The extractor's stdout did not parse as the diagnostics report — a
        // toolchain/version problem, not the user's code.
        CompileError::MalformedDiagnostics(msg) => {
            FailureEnvelope::new(FailureClass::VersionSkew, Phase::Compile, msg.clone())
        }
        // The user's result binding has IO type — a source-level constraint.
        CompileError::IOTypeDetected => {
            FailureEnvelope::new(FailureClass::UserHaskell, Phase::Compile, err.to_string())
        }
        // Spawn/IO failure reaching the extractor.
        CompileError::Io(_)
        | CompileError::EntryPreparationUnfinished { .. }
        | CompileError::EntryReservationReleaseUnconfirmed { .. }
        | CompileError::EntryPublicationUnconfirmed { .. } => {
            FailureEnvelope::new(FailureClass::Infra, Phase::Compile, err.to_string())
        }
        // The extractor produced no `.cbor`/`meta.cbor`.
        CompileError::MissingOutput(_) => {
            FailureEnvelope::new(FailureClass::Infra, Phase::Compile, err.to_string())
        }
        // The reader rejected the extractor's wire format.
        CompileError::ReadError(re) => FailureEnvelope::new(
            FailureClass::VersionSkew,
            Phase::Compile,
            version_skew_message(re),
        ),
        CompileError::Prepared(_) => {
            FailureEnvelope::new(FailureClass::VersionSkew, Phase::Compile, err.to_string())
        }
        // The extractor wrote an `asks.json` sidecar this reader could not
        // parse — the same "wire artifact this reader can't read" story as
        // `ReadError`, just for the typed-yield sidecar instead of the CBOR.
        CompileError::Asks(_) => {
            FailureEnvelope::new(FailureClass::VersionSkew, Phase::Compile, err.to_string())
        }
        // A prepared program's constructor host_id does not correspond to
        // the DataConTable entry shipped alongside it — cross-paired build
        // artifacts (e.g. a cache/session mismatch) or an extractor
        // identity-minting bug, never the user's source.
        CompileError::ConstructorIdentity(_) => {
            FailureEnvelope::new(FailureClass::VersionSkew, Phase::Compile, err.to_string())
        }
    };
    envelope.cause = Some(match err {
        CompileError::Io(_)
        | CompileError::EntryPreparationUnfinished { .. }
        | CompileError::EntryReservationReleaseUnconfirmed { .. }
        | CompileError::EntryPublicationUnconfirmed { .. } => CompileFailureCause::Io,
        CompileError::ExtractFailed(_) => CompileFailureCause::ExtractorContract,
        CompileError::CompilerEvidence(error) => CompileFailureCause::CompilerEvidence {
            failure: error.as_ref().into(),
        },
        CompileError::ArtifactInventory(error) => CompileFailureCause::ArtifactInventory {
            failure: error.failure.clone(),
        },
        CompileError::CompileInput(error) => CompileFailureCause::CompileInput {
            failure: error.clone(),
        },
        CompileError::ModulePackage(error) => CompileFailureCause::ModulePackage {
            failure: error.into(),
        },
        CompileError::InputRejected(_) => CompileFailureCause::InputRejected,
        CompileError::Diagnostics(_) => CompileFailureCause::SourceDiagnostics,
        CompileError::WorkerFailure(_) => CompileFailureCause::WorkerFailure,
        CompileError::MalformedDiagnostics(_) => CompileFailureCause::MalformedDiagnostics,
        CompileError::ReadError(_) => CompileFailureCause::WireDecode,
        CompileError::Prepared(_) => CompileFailureCause::PreparedArtifact,
        CompileError::MissingOutput(_) => CompileFailureCause::MissingOutput,
        CompileError::Asks(_) => CompileFailureCause::TypedSites,
        CompileError::IOTypeDetected => CompileFailureCause::UnsupportedIo,
        CompileError::ConstructorIdentity(_) => CompileFailureCause::ConstructorIdentity,
    });
    envelope
}

/// A self-diagnosing version-skew report: names the wire version seen (when the
/// reader could read one) against the version this server supports, and says to
/// redeploy/reconnect. Deliberately carries NO build sha — the actionable fact
/// is the wire versions and that the two sides disagree.
fn version_skew_message(re: &ReadError) -> String {
    let supported = format!("{VERSION_MAJOR}.{VERSION_MINOR}");
    let seen = match re {
        ReadError::UnsupportedVersion(major, minor) => format!("{major}.{minor}"),
        _ => "unrecognized (missing/foreign header)".to_string(),
    };
    format!(
        "Wire-format version skew: the extractor emitted Tidepool CBOR this \
         server's reader rejected (wire version seen: {seen}; this server \
         supports: {supported}). Underlying: {re}. The extractor and server \
         are built against different wire formats — redeploy the extractor and \
         reconnect the server so both sides run the same build."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn completed_source_evidence_keeps_typed_refusal_in_failure_envelope() {
        let input = PathBuf::from("/retained/source.hs");
        let failure = crate::cache::DependencyEvidenceFailure::Resolution {
            index: 85,
            reason: crate::cache::ResolutionWitnessFailure::NegativeCandidateUnavailable {
                index: 16,
            },
        };
        let error = CompileError::CompilerEvidence(Box::new(
            crate::certified_products::CertificationError::CompletedSourceEvidence {
                input: input.clone(),
                failure: Box::new(failure.clone()),
            },
        ));
        let envelope = classify_compile(&error);
        assert_eq!(envelope.class, FailureClass::Infra);
        assert_eq!(
            envelope.cause,
            Some(CompileFailureCause::CompilerEvidence {
                failure: CompilerEvidenceFailure::SourceEvidence { input, failure },
            })
        );
        let serialized = serde_json::to_value(envelope).unwrap();
        assert_eq!(
            serialized["cause"]["failure"]["failure"]["Resolution"]["index"],
            85
        );
        assert_eq!(
            serialized["cause"]["failure"]["failure"]["Resolution"]["reason"]
                ["NegativeCandidateUnavailable"]["index"],
            16
        );
    }

    #[test]
    fn compiler_evidence_read_and_resource_causes_keep_infra_classification() {
        use crate::certified_products::{
            CertificationError, EvidenceReadFailure, EvidenceReadOperation,
        };
        use tidepool_repr::execution_schema::ParseError;
        let path = PathBuf::from("/retained/receipt.cbor");
        for failure in [
            EvidenceReadFailure::SizeLimit {
                actual: 4208296,
                limit: 4194304,
            },
            EvidenceReadFailure::NotFile,
            EvidenceReadFailure::LengthChanged {
                expected: 5,
                actual: 6,
            },
            EvidenceReadFailure::Io {
                operation: EvidenceReadOperation::Metadata,
                error: std::io::Error::from(std::io::ErrorKind::NotFound),
            },
        ] {
            let error =
                CompileError::CompilerEvidence(Box::new(CertificationError::EvidenceRead {
                    path: path.clone(),
                    failure,
                }));
            let envelope = classify_compile(&error);
            assert_eq!(
                (envelope.class, envelope.phase),
                (FailureClass::Infra, Phase::Compile)
            );
            assert!(envelope.message.contains(path.to_str().unwrap()));
            let json = serde_json::to_value(envelope).unwrap();
            assert_eq!(json["cause"]["kind"], "compiler_evidence");
            assert_eq!(json["cause"]["failure"]["path"], path.to_str().unwrap());
        }
        for failure in [
            ParseError::InventoryByteLimit {
                limit: 4,
                actual: 5,
            },
            ParseError::ModuleByteLimit {
                limit: 4,
                actual: 5,
            },
            ParseError::ByteLimit {
                limit: 4,
                actual: 5,
            },
            ParseError::LimitExceeded("work"),
        ] {
            let error =
                CompileError::CompilerEvidence(Box::new(CertificationError::Product(failure)));
            let envelope = classify_compile(&error);
            assert_eq!(envelope.class, FailureClass::Infra);
            let json = serde_json::to_value(envelope).unwrap();
            assert_eq!(json["cause"]["kind"], "compiler_evidence");
            assert_ne!(json["cause"]["failure"]["kind"], "contract");
        }
        use crate::recovery_artifacts::{RecoveryAdmissionFailure, RecoveryArtifactError};
        for failure in [
            RecoveryAdmissionFailure::Decode(ParseError::LimitExceeded("work")),
            RecoveryAdmissionFailure::Decode(ParseError::ByteLimit {
                actual: 5,
                limit: 4,
            }),
            RecoveryAdmissionFailure::CertificateSize {
                format: crate::certified_products::CertificationFormat::CanonicalModuleCertificate,
                actual: 5,
                limit: 4,
            },
            RecoveryAdmissionFailure::EvidenceSize {
                path: path.clone(),
                actual: 5,
                limit: 4,
            },
        ] {
            let error = CompileError::CompilerEvidence(Box::new(
                CertificationError::CapturedModulePayload(
                    RecoveryArtifactError::InventoryAccounting(failure),
                ),
            ));
            let envelope = classify_compile(&error);
            assert_eq!(envelope.class, FailureClass::Infra);
            let json = serde_json::to_value(envelope).unwrap();
            assert_ne!(json["cause"]["failure"]["kind"], "contract");
        }
        for failure in [
            CertificationError::Product(ParseError::TrailingBytes),
            CertificationError::Receipt("receipt shape"),
            CertificationError::EvidenceRead {
                path,
                failure: EvidenceReadFailure::NonAbsolutePath,
            },
        ] {
            let error = CompileError::CompilerEvidence(Box::new(failure));
            assert_eq!(classify_compile(&error).class, FailureClass::VersionSkew);
        }
    }

    #[test]
    fn input_and_package_proof_failures_keep_structured_owners() {
        let input = classify_compile(&CompileError::CompileInput(
            crate::CompileInputError::MissingPackageImport {
                unit: "main".into(),
                module: "Checked".into(),
                imported: "Facade".into(),
            },
        ));
        let input = serde_json::to_value(input).unwrap();
        assert_eq!(input["cause"]["kind"], "compile_input");
        assert_eq!(input["cause"]["failure"]["kind"], "missing_package_import");
        assert_eq!(input["cause"]["failure"]["module"], "Checked");
        let package = classify_compile(&CompileError::ModulePackage(
            crate::toolchain::ModulePackageError::IncompleteProduct {
                unit: "main".into(),
                module: "Support".into(),
                availability: crate::cache::ProductAvailability::InterfaceOnly,
            },
        ));
        let package = serde_json::to_value(package).unwrap();
        assert_eq!(package["cause"]["kind"], "module_package");
        assert_eq!(package["cause"]["failure"]["module"], "Support");
        assert_eq!(
            package["cause"]["failure"]["availability"],
            "interface_only"
        );
    }

    #[test]
    fn artifact_inventory_diagnostic_retains_typed_owner_edge() {
        use crate::artifact_inventory::{
            ArtifactDependency, ArtifactId, ArtifactInventoryError, ArtifactInventoryFailure,
        };
        use crate::declaration_join::ExactModuleIdentity;
        let failure = ArtifactInventoryFailure::MissingDependency {
            artifact: ArtifactId([7; 32]),
            dependent: ExactModuleIdentity {
                unit: "main".into(),
                module: "Dependent".into(),
            },
            required: ExactModuleIdentity {
                unit: "package".into(),
                module: "Original".into(),
            },
            dependency: ArtifactDependency::NativeBinding {
                dependent_ordinal: 23,
                generation: 0,
                namespace: "value".into(),
                occurrence: "map".into(),
                record_parent: None,
            },
        };
        let error = CompileError::ArtifactInventory(ArtifactInventoryError {
            failure: failure.clone(),
            diagnostic_artifacts: Some(PathBuf::from("retained-evidence")),
            owner_conflict: None,
        });
        let diagnostic = classify_compile(&error);
        assert_eq!(diagnostic.class, FailureClass::VersionSkew);
        assert_eq!(diagnostic.phase, Phase::Compile);
        assert_eq!(
            diagnostic.cause,
            Some(CompileFailureCause::ArtifactInventory { failure })
        );
        assert!(diagnostic.message.contains("retained-evidence"));
        let metadata = serde_json::to_value(&diagnostic).unwrap();
        assert_eq!(metadata["class"], "version-skew");
        assert_eq!(
            metadata["cause"]["failure"]["required"]["module"],
            "Original"
        );
        assert_eq!(
            metadata["cause"]["failure"]["dependency"]["native_binding"]["dependent_ordinal"],
            23
        );
        assert!(
            metadata.get("message").is_none(),
            "human diagnostic must not be duplicated in metadata"
        );
    }

    #[test]
    fn extractor_contract_failure_is_version_skew_compile() {
        let env = classify_compile(&CompileError::ExtractFailed(
            "TurnOut CBOR: malformed".into(),
        ));
        assert_eq!(env.class, FailureClass::VersionSkew);
        assert_eq!(env.phase, Phase::Compile);
    }

    #[test]
    fn io_type_is_user_haskell_compile() {
        let env = classify_compile(&CompileError::IOTypeDetected);
        assert_eq!(env.class, FailureClass::UserHaskell);
        assert_eq!(env.phase, Phase::Compile);
    }

    #[test]
    fn spawn_io_is_infra_compile() {
        let env = classify_compile(&CompileError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "tidepool-extract not found on PATH",
        )));
        assert_eq!(env.class, FailureClass::Infra);
        assert_eq!(env.phase, Phase::Compile);
    }

    #[test]
    fn compiler_worker_failure_is_infra_compile() {
        let env = classify_compile(&CompileError::WorkerFailure(vec![]));
        assert_eq!(env.class, FailureClass::Infra);
        assert_eq!(env.phase, Phase::Compile);
    }

    #[test]
    fn missing_output_is_infra_compile() {
        let env = classify_compile(&CompileError::MissingOutput(PathBuf::from("result.cbor")));
        assert_eq!(env.class, FailureClass::Infra);
        assert_eq!(env.phase, Phase::Compile);
    }

    /// The motivating bug: a wire-format skew must classify as version-skew /
    /// compile, NOT user-haskell — a caller branching on class must be routed to
    /// "redeploy", not "rewrite your code".
    #[test]
    fn missing_header_is_version_skew_compile_not_user_haskell() {
        let env = classify_compile(&CompileError::ReadError(ReadError::MissingHeader));
        assert_eq!(env.class, FailureClass::VersionSkew);
        assert_eq!(env.phase, Phase::Compile);
        assert_ne!(env.class, FailureClass::UserHaskell);
    }

    /// The skew message is self-diagnosing: it names the wire version seen vs.
    /// the supported version and says to redeploy — and carries no build sha.
    #[test]
    fn unsupported_version_message_names_versions_and_no_sha() {
        let env = classify_compile(&CompileError::ReadError(ReadError::UnsupportedVersion(
            9, 7,
        )));
        assert_eq!(env.class, FailureClass::VersionSkew);
        assert!(env.message.contains("9.7"), "names the version seen");
        assert!(
            env.message
                .contains(&format!("{VERSION_MAJOR}.{VERSION_MINOR}")),
            "names the supported version"
        );
        assert!(
            env.message.to_lowercase().contains("redeploy"),
            "says to redeploy"
        );
        assert!(
            !env.message.to_lowercase().contains("sha"),
            "carries no build sha"
        );
    }

    #[test]
    fn tags_are_stable() {
        assert_eq!(FailureClass::UserHaskell.tag(), "user-haskell");
        assert_eq!(FailureClass::Runtime.tag(), "runtime");
        assert_eq!(FailureClass::Infra.tag(), "infra");
        assert_eq!(FailureClass::VersionSkew.tag(), "version-skew");
        assert_eq!(Phase::Compile.tag(), "compile");
        assert_eq!(Phase::Install.tag(), "install");
        assert_eq!(Phase::Run.tag(), "run");
    }
}
