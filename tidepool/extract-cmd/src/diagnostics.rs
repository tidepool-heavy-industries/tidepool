//! Recognized `tidepool-*` machine-readable stderr prefixes.
//!
//! The process boundary and compiler worker (`bridge/haskell/src/Tidepool/Timing.hs` and
//! `GhcPipeline.hs`) writes these lines to stderr under `TIDEPOOL_TIMING=1`
//! and during structural module accounting. They are measurements for the
//! daemon's detailed log, never source diagnostics, so every consumer that
//! renders or logs worker stderr filters them with this SAME list — the
//! daemon's own transaction log (`tidepool-extract-cmd`) and the
//! human-facing diagnostic renderer (`tidepool-toolchain`). This module is
//! the one place the list is spelled; extend it here, not by copying it.

/// Every `tidepool-*` stderr prefix the compiler worker is known to emit for
/// machine-readable measurement/accounting lines (as opposed to a GHC source
/// diagnostic). A worker line starting with one of these, after leading
/// whitespace is trimmed, is machine-readable and never a source diagnostic.
pub const MACHINE_STDERR_PREFIXES: [&str; 25] = [
    "tidepool-build-products ",
    "tidepool-timing ",
    "tidepool-timing-detail ",
    "tidepool-timing-module ",
    "tidepool-timing-module-detail ",
    "tidepool-count ",
    "tidepool-reuse ",
    "tidepool-validation ",
    "tidepool-meta-execution ",
    "tidepool-compile-summary ",
    "tidepool-memo-miss ",
    "tidepool-checked ",
    "tidepool-checked-loaded-source ",
    "tidepool-checked-reused-source ",
    "tidepool-candidate-admission ",
    "tidepool-canonical-frontend ",
    "tidepool-canonical-finalization ",
    "tidepool-checked-dependency-executable ",
    "tidepool-checked-interface-retained ",
    "tidepool-checked-interface-elided ",
    "tidepool-dependency-witness ",
    "tidepool-dependent-files ",
    "tidepool-target ",
    "tidepool-memo-cycle-graph ",
    "tidepool-memo-trace-miss ",
];

/// Whether `line` (after trimming leading whitespace) starts with a
/// recognized machine-readable prefix.
pub fn is_machine_stderr_line(line: &str) -> bool {
    MACHINE_STDERR_PREFIXES
        .iter()
        .any(|prefix| line.trim_start().starts_with(prefix))
}

#[cfg(test)]
#[path = "diagnostics_tests.rs"]
mod tests;
