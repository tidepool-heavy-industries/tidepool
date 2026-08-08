//! Per-turn latency attribution: the stage vocabulary, the extract-side timing
//! wire format, and the one emitter every instrumented call site uses.
//!
//! # Why a module and not scattered `debug!`s
//!
//! A turn's wall clock is split across three processes' worth of work (the
//! `tidepool-extract` subprocess, the Rust-side deserialize, the JIT machine),
//! and the bench harness has to reassemble those into one attribution table. A
//! collector can only do that if every stage arrives under the SAME event
//! shape, so stages are emitted through [`record_stage`] rather than by hand.
//!
//! # The event shape
//!
//! One `tracing` DEBUG event per stage, on target `tidepool_harness::timing`,
//! message `"turn stage"`, fields:
//!
//! | field   | type          | meaning                                        |
//! |---------|---------------|------------------------------------------------|
//! | `node`  | `u64`         | the [`crate::NodeId`] the stage ran for         |
//! | `round` | `u64`         | answerer round index (`u64::MAX` = not a round) |
//! | `stage` | `&str`        | one of the `STAGE_*` constants below            |
//! | `ms`    | `u64`         | wall-clock milliseconds for that stage          |
//! | `bytes` | `u64`         | size of the stage's payload, `0` when n/a       |
//!
//! Stages are FLAT and non-nesting: a collector sums by `stage` and never has
//! to reason about containment. Where a coarse stage contains finer ones, the
//! fine stages are prefixed by WHICH `tidepool-extract` spawn they came from —
//! a round makes TWO separate spawns (see `plans/self-iterating-harness/
//! 11-turn-latency-contract.md`'s pipeline walk): `extract.*` is the inside of
//! the compile lane's `extract_spawn`, and `classify.*` is the inside of the
//! parse-only classify lane's `classify_extract`. Keeping them under distinct
//! prefixes matters beyond bookkeeping: it is the only way to see whether the
//! classify spawn is almost entirely GHC-session boot or does real work. A
//! collector summing `extract.ghc_session` must never fold in
//! `classify.ghc_session`'s numbers — that would silently merge two different
//! subprocess spawns into one row and make the two lanes' costs unreadable.
//!
//! # The extract-side wire format
//!
//! `tidepool-extract` is a subprocess, so its internal phases cannot be tracing
//! events. With [`TIMING_ENV`] set to `1` it writes one line per phase to
//! STDERR:
//!
//! ```text
//! tidepool-timing phase=<name> ms=<integer>
//! ```
//!
//! Anything else on stderr is ignored by [`ExtractTiming::parse`]. The lines
//! are DIAGNOSTIC ONLY — stdout (the JSON diagnostics report) and the emitted
//! CBOR are the wire contract and must be byte-identical with and without the
//! env var set. With the var unset the extract emits nothing at all.

use std::time::Duration;

/// Env var that turns on extract-side phase timing (`=1`). Off ⇒ silent.
pub const TIMING_ENV: &str = "TIDEPOOL_TIMING";

/// Line prefix of an extract-side timing line on stderr.
pub const TIMING_PREFIX: &str = "tidepool-timing ";

/// `round` value meaning "this stage is not inside a numbered answerer round".
pub const NO_ROUND: u64 = u64::MAX;

/// `node` value meaning "this stage has no answerer node of its own" (a boot
/// compile, an outer-session compile, a resident-session mirror). `NodeId(0)`
/// is a REAL, LIVE node id (`forcing.rs`'s `next_node_id` starts at 0) — never
/// reuse bare `0` as a stand-in for "no node" or its samples land on that
/// node's numbers.
pub const NO_NODE: u64 = u64::MAX;

// ---------------------------------------------------------------------------
// Stage vocabulary — Rust side
// ---------------------------------------------------------------------------

/// Model inference: the provider call that produces the turn's reply.
pub const STAGE_PROVIDER_CALL: &str = "provider_call";
/// The parse-only `--emit-stmt-binders` extract subprocess (turn classification).
pub const STAGE_CLASSIFY_EXTRACT: &str = "classify_extract";
/// Assembling the templated module source for the block.
pub const STAGE_TEMPLATE: &str = "template";
/// The full compile extract subprocess, spawn to exit (contains `extract.*`).
pub const STAGE_EXTRACT_SPAWN: &str = "extract_spawn";
/// Reading the extract's output files off disk.
pub const STAGE_CBOR_READ: &str = "cbor_read";
/// `read_cbor` + `read_metadata` — CBOR → `CoreExpr`/`DataConTable`.
pub const STAGE_CBOR_DESERIALIZE: &str = "cbor_deserialize";
/// Parsing the `asks.json` sidecar.
pub const STAGE_ASKS_PARSE: &str = "asks_parse";
/// Cranelift codegen: minting the fragment into the resident machine.
pub const STAGE_JIT_CODEGEN: &str = "jit_codegen";
/// Running the compiled fragment on the machine (to completion or suspension).
pub const STAGE_RUN_EXEC: &str = "run_exec";

/// Every Rust-side stage, in pipeline order — the attribution table's row order.
pub const RUST_STAGES: &[&str] = &[
    STAGE_PROVIDER_CALL,
    STAGE_CLASSIFY_EXTRACT,
    STAGE_TEMPLATE,
    STAGE_EXTRACT_SPAWN,
    STAGE_CBOR_READ,
    STAGE_CBOR_DESERIALIZE,
    STAGE_ASKS_PARSE,
    STAGE_JIT_CODEGEN,
    STAGE_RUN_EXEC,
];

// ---------------------------------------------------------------------------
// Stage vocabulary — extract side (phase names on the stderr lines)
// ---------------------------------------------------------------------------

/// Process start to the point the GHC session is about to be created.
pub const PHASE_STARTUP: &str = "startup";
/// Creating the GHC session: flag parsing, package-db + interface loading.
pub const PHASE_GHC_SESSION: &str = "ghc_session";
/// Parse + rename + typecheck of the turn module (and any `--include` modules).
pub const PHASE_TYPECHECK: &str = "typecheck";
/// Desugar to Core + the simplifier passes GHC runs before we read binds.
pub const PHASE_CORE: &str = "core";
/// `Tidepool.Translate`: GHC Core → our `CoreExpr` + `DataConTable`.
pub const PHASE_TRANSLATE: &str = "translate";
/// `Tidepool.CborEncode`: serializing the tree + metadata.
pub const PHASE_CBOR_ENCODE: &str = "cbor_encode";
/// Writing `<target>.cbor` / `meta.cbor` / `asks.json`.
pub const PHASE_WRITE: &str = "write";
/// Whole-process wall clock as the extract itself measures it.
pub const PHASE_TOTAL: &str = "total";

/// Every extract-side phase, in pipeline order.
pub const EXTRACT_PHASES: &[&str] = &[
    PHASE_STARTUP,
    PHASE_GHC_SESSION,
    PHASE_TYPECHECK,
    PHASE_CORE,
    PHASE_TRANSLATE,
    PHASE_CBOR_ENCODE,
    PHASE_WRITE,
    PHASE_TOTAL,
];

/// Prefix a forwarded COMPILE-lane phase is emitted under.
pub const EXTRACT_STAGE_PREFIX: &str = "extract.";

/// Prefix a forwarded CLASSIFY-lane phase is emitted under. Distinct from
/// [`EXTRACT_STAGE_PREFIX`] — see the module doc's flat-stages section for why
/// the two lanes must never share a prefix.
pub const CLASSIFY_STAGE_PREFIX: &str = "classify.";

/// The stage name a forwarded COMPILE-lane extract phase is emitted under
/// (`extract.<phase>`) — the inside of `extract_spawn`.
pub fn extract_stage_name(phase: &str) -> String {
    format!("{EXTRACT_STAGE_PREFIX}{phase}")
}

/// The stage name a forwarded CLASSIFY-lane extract phase is emitted under
/// (`classify.<phase>`) — the inside of `classify_extract`. Kept under its own
/// prefix so the parse-only classify spawn's internal phases never merge with
/// the full compile spawn's when a collector sums by stage name.
pub fn classify_stage_name(phase: &str) -> String {
    format!("{CLASSIFY_STAGE_PREFIX}{phase}")
}

// ---------------------------------------------------------------------------
// Emitter
// ---------------------------------------------------------------------------

/// Emit one stage event. `bytes` is the stage's payload size where one is
/// meaningful (source length, CBOR length), `0` otherwise.
pub fn record_stage(node: u64, round: u64, stage: &str, elapsed: Duration, bytes: u64) {
    tracing::debug!(
        target: "tidepool_harness::timing",
        node,
        round,
        stage,
        ms = elapsed.as_millis() as u64,
        bytes,
        "turn stage"
    );
}

/// Emit every phase parsed out of an extract's stderr as an `extract.*` stage.
pub fn record_extract_phases(node: u64, round: u64, timing: &ExtractTiming) {
    for (phase, ms) in &timing.phases {
        record_stage(
            node,
            round,
            &extract_stage_name(phase),
            Duration::from_millis(*ms),
            0,
        );
    }
}

// ---------------------------------------------------------------------------
// Extract-side stderr parsing
// ---------------------------------------------------------------------------

/// Phase timings parsed from a `tidepool-extract` stderr stream. Empty when the
/// extract ran without [`TIMING_ENV`] (or predates the timing lines) — an empty
/// result is NOT an error, it just means no extract-side attribution.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtractTiming {
    /// `(phase, milliseconds)` in the order the extract emitted them.
    pub phases: Vec<(String, u64)>,
}

impl ExtractTiming {
    /// Scan `stderr` for `tidepool-timing phase=<name> ms=<int>` lines.
    /// Malformed lines are skipped: this is diagnostics, never a failure path.
    pub fn parse(stderr: &str) -> Self {
        let mut phases = Vec::new();
        for line in stderr.lines() {
            let Some(rest) = line.trim().strip_prefix(TIMING_PREFIX) else {
                continue;
            };
            let mut phase = None;
            let mut ms = None;
            for field in rest.split_whitespace() {
                if let Some(v) = field.strip_prefix("phase=") {
                    phase = Some(v.to_string());
                } else if let Some(v) = field.strip_prefix("ms=") {
                    ms = v.parse::<u64>().ok();
                }
            }
            if let (Some(p), Some(m)) = (phase, ms) {
                phases.push((p, m));
            }
        }
        ExtractTiming { phases }
    }

    /// Milliseconds recorded for `phase`, if the extract emitted it.
    pub fn get(&self, phase: &str) -> Option<u64> {
        self.phases
            .iter()
            .find(|(p, _)| p == phase)
            .map(|(_, ms)| *ms)
    }

    pub fn is_empty(&self) -> bool {
        self.phases.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_timing_lines_and_ignores_noise() {
        let stderr = "\
some ghc warning\n\
tidepool-timing phase=ghc_session ms=1420\n\
tidepool-timing phase=typecheck ms=310\n\
tidepool-timing garbage\n\
tidepool-timing phase=total ms=2100\n";
        let t = ExtractTiming::parse(stderr);
        assert_eq!(t.phases.len(), 3);
        assert_eq!(t.get(PHASE_GHC_SESSION), Some(1420));
        assert_eq!(t.get(PHASE_TYPECHECK), Some(310));
        assert_eq!(t.get(PHASE_TOTAL), Some(2100));
        assert_eq!(t.get(PHASE_TRANSLATE), None);
    }

    #[test]
    fn no_timing_lines_parses_empty() {
        assert!(ExtractTiming::parse("plain stderr\n").is_empty());
    }

    #[test]
    fn extract_stage_names_are_prefixed() {
        assert_eq!(extract_stage_name(PHASE_TYPECHECK), "extract.typecheck");
    }

    #[test]
    fn classify_stage_names_use_a_distinct_prefix() {
        assert_eq!(classify_stage_name(PHASE_TYPECHECK), "classify.typecheck");
        assert_ne!(
            classify_stage_name(PHASE_TYPECHECK),
            extract_stage_name(PHASE_TYPECHECK)
        );
    }
}
