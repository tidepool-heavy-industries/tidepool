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
//! a harness turn makes exactly ONE spawn (see `plans/self-iterating-harness/
//! 11-extract-timing-contract.md`'s pipeline walk): `extract.*` is the inside
//! of that single `--turn` spawn's `extract_spawn`, including its own
//! in-process `classify` substep (`extract.classify`). `classify.*` is a
//! DIFFERENT lane's prefix — the block-classify spawn `tidepool_runtime::
//! session::classify_block` makes on the repl's behalf, not something a
//! harness turn emits at all. Keeping them under distinct prefixes matters
//! beyond bookkeeping: it is the only way to see whether the extract's
//! in-process classify substep is almost entirely GHC-session boot or does
//! real work. `extract.*` never carries a `ghc_session` row post-partition
//! (see [`PHASE_GHC_SESSION`]'s doc) — only `classify.ghc_session` does — but
//! the rule generalizes: a collector must never fold ANY `extract.<phase>`
//! into the like-named `classify.<phase>`, since they are two different
//! subprocess spawns even where a name happens to coincide.
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
//!
//! # Flat phase partition (compile lane)
//!
//! On BOTH extract pipeline paths (`runNormalPipeline` and
//! `runSessionPipeline` in `haskell/src/Tidepool/GhcPipeline.hs`),
//! [`PHASE_GHC_SETUP`] (session `DynFlags` setup + `guessTarget`/`setTargets`
//! + `depanal`) and [`PHASE_GHC_LOAD`] (the `load'` call alone) are two
//! SEPARATE, NON-OVERLAPPING spans — not one nested inside the other. They
//! PARTITION what an older, now-retired `ghc_session` bracket used to cover
//! on the compile lane; see [`PHASE_GHC_SESSION`]'s doc for the retirement.
//! A collector recovers the old coarse figure as the SUM `ghc_setup +
//! ghc_load` — a flat-sum collector already does this for free, and neither
//! row is emitted twice, so there is nothing to avoid double-counting. On the
//! session path, [`PHASE_INJECT`] (PHASE 2's Val-iface splice) is a third
//! flat row alongside them, with no normal-path counterpart. `load'` itself
//! gets NO internal decomposition: it already redoes the SAME
//! parse/typecheck/core2core work the per-module loop below it redoes a
//! second time (see [`PHASE_TYPECHECK`]/[`PHASE_CORE`]), so one row around
//! the whole call answers what matters. This is
//! `haskell/src/Tidepool/Timing.hs`'s flat-phase doc, restated here since the
//! two modules must stay in sync by hand.

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
/// `ghc_session` now denotes exactly ONE span — the `--classify` lane's
/// `getSessionDynFlags` (`Binders.hs` `classifyBlock`). The compile lane's
/// former, much larger use (session setup + `depanal` + `load'`) is
/// succeeded by [`PHASE_GHC_SETUP`] + [`PHASE_GHC_LOAD`]; a historical
/// compile-lane `ghc_session` equals their sum. This is the SAME retirement
/// discipline as the `classify_extract` tombstone (see the module doc): a
/// same-named phase never changes meaning, so `ghc_session` keeps its
/// ORIGINAL (small, classify-lane) meaning rather than being repurposed, and
/// the compile lane's much larger span gets two new names instead.
pub const PHASE_GHC_SESSION: &str = "ghc_session";
/// FLAT (compile lane, both paths): session `DynFlags` setup +
/// `guessTarget`/`setTargets` + the `depanal` call alone. Partitions the
/// retired compile-lane `ghc_session` together with [`PHASE_GHC_LOAD`] — see
/// [`PHASE_GHC_SESSION`]'s doc.
pub const PHASE_GHC_SETUP: &str = "ghc_setup";
/// FLAT (compile lane, both paths): GHC's `load' LoadAllTargets` call alone
/// — a full compile of every home module (including `core2core`), separate
/// from the SECOND parse/typecheck/core2core loop that
/// [`PHASE_TYPECHECK`]/[`PHASE_CORE`] measure. Gets NO internal
/// decomposition — see the module doc's flat-phase-partition section for why
/// one row around the whole call is what answers C1.
pub const PHASE_GHC_LOAD: &str = "ghc_load";
/// FLAT, SESSION-PATH ONLY (absent on `runNormalPipeline`, which never
/// injects session Vals): `injectSessionScope` splicing the live `Val.G<g>`
/// ifaces into the HPT.
pub const PHASE_INJECT: &str = "inject";
/// The `--turn` mode's in-process classify substep (GHC-sourced verdict,
/// inside the booted session, before any compile work) — absent when a
/// caller supplies `--turn-verdict` and the mode skips its own re-parse.
pub const PHASE_CLASSIFY: &str = "classify";
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

/// Every extract-side phase, in pipeline order, across BOTH lanes. All
/// FLAT — no entry is summed into another; see [`PHASE_GHC_SESSION`]'s doc
/// for why `ghc_session` (classify lane only) sits apart from `ghc_setup`/
/// `ghc_load` (compile lane) despite the adjacent listing.
pub const EXTRACT_PHASES: &[&str] = &[
    PHASE_STARTUP,
    PHASE_GHC_SETUP,
    PHASE_GHC_LOAD,
    PHASE_GHC_SESSION,
    PHASE_INJECT,
    PHASE_CLASSIFY,
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
/// (`classify.<phase>`) — the inside of `tidepool_runtime::session::classify_block`'s
/// batch spawn (not something a harness turn makes; the repl's block runner
/// is this prefix's only caller). Kept under its own prefix so that spawn's
/// internal phases never merge with a `--turn` spawn's `extract.*` phases
/// (which include their OWN in-process `extract.classify` substep) when a
/// collector sums by stage name.
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
    fn parses_the_flat_ghc_setup_and_ghc_load_rows() {
        let stderr = "\
tidepool-timing phase=ghc_setup ms=142\n\
tidepool-timing phase=ghc_load ms=4533\n";
        let t = ExtractTiming::parse(stderr);
        assert_eq!(t.phases.len(), 2);
        assert_eq!(t.get(PHASE_GHC_SETUP), Some(142));
        assert_eq!(t.get(PHASE_GHC_LOAD), Some(4533));
        // Post-partition the compile lane never emits PHASE_GHC_SESSION —
        // only the classify lane (Binders.hs classifyBlock) does.
        assert_eq!(t.get(PHASE_GHC_SESSION), None);
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
