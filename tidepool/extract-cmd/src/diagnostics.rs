//! Recognized `tidepool-*` machine-readable stderr prefixes.
//!
//! The compiler worker (`bridge/haskell/src/Tidepool/Timing.hs` and
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
pub const MACHINE_STDERR_PREFIXES: [&str; 15] = [
    "tidepool-timing ",
    "tidepool-timing-detail ",
    "tidepool-timing-module ",
    "tidepool-timing-module-detail ",
    "tidepool-count ",
    "tidepool-compile-summary ",
    "tidepool-memo-miss ",
    "tidepool-checked ",
    "tidepool-checked-dependency-executable ",
    "tidepool-checked-interface-retained ",
    "tidepool-checked-interface-elided ",
    "tidepool-dependency-witness ",
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
mod tests {
    use super::MACHINE_STDERR_PREFIXES;
    use std::collections::BTreeSet;

    /// Scans a Haskell source file for every `"tidepool-<word> "` literal —
    /// a quoted string starting with `tidepool-` whose first token (up to
    /// the next space, found strictly before the closing quote) is the
    /// emitted line's prefix. This is a scan of the literal text the worker
    /// actually writes, not a copy of this module's own list, so a renamed
    /// or newly added emitter is picked up and a removed one is caught as a
    /// stale entry here.
    fn emitted_prefixes(source: &str) -> BTreeSet<String> {
        let mut found = BTreeSet::new();
        let mut cursor = 0usize;
        while let Some(rel) = source[cursor..].find("\"tidepool-") {
            let word_start = cursor + rel + 1; // past the opening quote
            let tail = &source[word_start..];
            let space = tail.find(' ');
            let quote = tail.find('"');
            if let (Some(space), Some(quote)) = (space, quote) {
                if space < quote {
                    found.insert(format!("{} ", &tail[..space]));
                }
            }
            cursor = word_start;
        }
        found
    }

    /// The daemon's and the human-facing renderer's shared prefix list must
    /// name exactly the machine-readable lines the compiler worker emits —
    /// no more (a stale prefix that filters nothing) and no fewer (a new
    /// emitter whose lines leak into a source diagnostic).
    #[test]
    fn machine_stderr_prefixes_match_the_haskell_emitters() {
        let timing = include_str!("../../../bridge/haskell/src/Tidepool/Timing.hs");
        let ghc_pipeline = include_str!("../../../bridge/haskell/src/Tidepool/GhcPipeline.hs");

        let mut emitted = emitted_prefixes(timing);
        emitted.extend(emitted_prefixes(ghc_pipeline));

        let declared: BTreeSet<String> = MACHINE_STDERR_PREFIXES
            .iter()
            .map(|prefix| prefix.to_string())
            .collect();

        assert_eq!(
            declared, emitted,
            "MACHINE_STDERR_PREFIXES has drifted from the tidepool-* literals \
             emitted by Timing.hs/GhcPipeline.hs"
        );
    }
}
