//! The session/JIT half of the eval failure-classification surface.
//!
//! [`FailureClass`], [`Phase`], [`FailureEnvelope`], and [`classify_compile`]
//! (the actual `CompileError` decision tree) live in
//! `tidepool_toolchain::failclass` and are re-exported here unchanged.
//! [`classify`] and [`classify_session`] stay in this crate because they
//! dispatch over [`RuntimeError`] and [`SessionError`] — types that wrap
//! JIT/session state and must not be visible to `tidepool-toolchain`, which
//! this crate depends on, not the reverse.

use crate::session::SessionError;
use crate::{CompileError, RuntimeError};

pub use tidepool_toolchain::failclass::{classify_compile, FailureClass, FailureEnvelope, Phase};

/// Dispatch [`classify_compile`] over the unified [`RuntimeError`]: a JIT error
/// is always a run-phase runtime failure; a compile error defers to the one
/// classifier above.
#[must_use]
pub fn classify(err: &RuntimeError) -> FailureEnvelope {
    match err {
        RuntimeError::Compile(c) => classify_compile(c),
        RuntimeError::Jit(_) => {
            FailureEnvelope::new(FailureClass::Runtime, Phase::Run, err.to_string())
        }
    }
}

/// The repl's declaration-accumulation path fails with a [`SessionError`]; its
/// compiler arm retains the original [`CompileError`] and defers directly to
/// the ONE classifier so declaration and evaluation paths agree.
#[must_use]
pub fn classify_session(err: &SessionError) -> FailureEnvelope {
    match err {
        SessionError::Io(e) => classify_compile(&CompileError::Io(std::io::Error::new(
            e.kind(),
            e.to_string(),
        ))),
        SessionError::Compile(e) => classify_compile(e),
        SessionError::ValidationFailed(s) => {
            FailureEnvelope::new(FailureClass::UserHaskell, Phase::Compile, s.to_string())
        }
        // A located-but-skewed toolchain is the same failure as a wire-format
        // mismatch — the two sides were built apart — so it classifies as
        // VersionSkew and inherits the "redeploy/reconnect" advice. Every other
        // toolchain misconfiguration (no extract, no stdlib, unwritable stamp)
        // is an environment problem: Infra.
        SessionError::Toolchain(t) => {
            let class = match t {
                crate::toolchain::ToolchainError::Skew(_) => FailureClass::VersionSkew,
                _ => FailureClass::Infra,
            };
            FailureEnvelope::new(class, Phase::Compile, t.to_string())
        }
        // A stale/forged `ScopeId` reaching a scope-taking mutation is a
        // caller bug (never the user's declaration, never an environment or
        // wire-format problem) surfacing while the engine is driving a turn —
        // the same "failed while running" shape `Runtime` already covers.
        SessionError::DeadScope(_) => {
            FailureEnvelope::new(FailureClass::Runtime, Phase::Run, err.to_string())
        }
        // Same shape as `DeadScope`: a caller bug in the mount seam (a stale
        // scope/name pair, or a placeholder bind that never ran), never the
        // user's declaration.
        SessionError::UnknownBinding { .. } => {
            FailureEnvelope::new(FailureClass::Runtime, Phase::Run, err.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tidepool_repr::serial::ReadError;

    /// The session decl lane agrees with the eval lane: an unparseable
    /// diagnostics report is a stale/skewed extractor build — VersionSkew,
    /// never "rewrite your Haskell".
    #[test]
    fn session_malformed_diagnostics_is_version_skew_compile() {
        let env = classify_session(&SessionError::Compile(CompileError::MalformedDiagnostics(
            "stdout did not parse as a diagnostics report".into(),
        )));
        assert_eq!(env.class, FailureClass::VersionSkew);
        assert_eq!(env.phase, Phase::Compile);
    }

    /// The session decl lane agrees with the eval lane: a spawn failure
    /// (`SessionError::Io(NotFound)`) is Infra — "install/point at the
    /// extractor", never "rewrite your Haskell".
    #[test]
    fn session_spawn_io_is_infra_compile() {
        let env = classify_session(&SessionError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "tidepool-extract not found on PATH",
        )));
        assert_eq!(env.class, FailureClass::Infra);
        assert_eq!(env.phase, Phase::Compile);
    }

    /// Crossing into `SessionError` must not collapse extractor artifacts into
    /// a declaration string: each original variant remains matchable and keeps
    /// the owning compile classifier's failure class.
    #[test]
    fn session_compile_boundary_preserves_artifact_variants() {
        let missing =
            SessionError::from(CompileError::MissingOutput(PathBuf::from("turn-out.cbor")));
        assert!(matches!(
            &missing,
            SessionError::Compile(CompileError::MissingOutput(_))
        ));
        assert_eq!(classify_session(&missing).class, FailureClass::Infra);

        let unreadable = SessionError::from(CompileError::ReadError(ReadError::MissingHeader));
        assert!(matches!(
            &unreadable,
            SessionError::Compile(CompileError::ReadError(ReadError::MissingHeader))
        ));
        assert_eq!(
            classify_session(&unreadable).class,
            FailureClass::VersionSkew
        );

        let asks = SessionError::from(CompileError::Asks("not JSON".into()));
        assert!(matches!(
            &asks,
            SessionError::Compile(CompileError::Asks(_))
        ));
        assert_eq!(classify_session(&asks).class, FailureClass::VersionSkew);
    }
}
