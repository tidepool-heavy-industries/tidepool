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
//! - **phase** — WHEN it failed: [`Phase::Compile`] (during source→Core
//!   extraction) or [`Phase::Run`] (during JIT execution).
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureClass {
    /// GHC parse/type/scope error in the user's code, or an unsupported IO
    /// result type — the extractor ran and rejected the source. Fix the code.
    UserHaskell,
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
            FailureClass::Runtime => "runtime",
            FailureClass::Infra => "infra",
            FailureClass::VersionSkew => "version-skew",
        }
    }
}

/// WHEN it failed — the compile→run boundary is machine creation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// During source→Core extraction (the `tidepool-extract` shell-out + read).
    Compile,
    /// During JIT execution of the compiled program.
    Run,
}

impl Phase {
    /// Stable lowercase tag, safe to grep and count.
    #[must_use]
    pub fn tag(self) -> &'static str {
        match self {
            Phase::Compile => "compile",
            Phase::Run => "run",
        }
    }
}

/// A classified failure: the two routing axes plus the human-facing message.
/// Successful evals never build one — this rides only the error path.
#[derive(Clone, Debug)]
pub struct FailureEnvelope {
    /// WHAT failed.
    pub class: FailureClass,
    /// WHEN it failed.
    pub phase: Phase,
    /// Human-facing message (already re-messaged for a version skew; the
    /// error's own `Display` text otherwise).
    pub message: String,
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
    match err {
        // Real GHC rejections carry `Diagnostics`. This arm is reserved for a
        // malformed extractor artifact or impossible internal request shape.
        CompileError::ExtractFailed(_) => {
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
        CompileError::Io(_) => {
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
        // The extractor wrote an `asks.json` sidecar this reader could not
        // parse — the same "wire artifact this reader can't read" story as
        // `ReadError`, just for the typed-yield sidecar instead of the CBOR.
        CompileError::Asks(_) => {
            FailureEnvelope::new(FailureClass::VersionSkew, Phase::Compile, err.to_string())
        }
    }
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
        assert_eq!(Phase::Run.tag(), "run");
    }
}
