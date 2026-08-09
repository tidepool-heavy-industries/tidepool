//! Turn compilation: Haskell source → `(CoreExpr, DataConTable, AsksSidecar)`.
//!
//! The turn engine compiles each model-written eval block the same way the
//! MCP server does (`template_haskell` wrapping + `tidepool-extract`), but the
//! harness ALSO needs the `asks.json` sidecar (#R0 typed-yield pass) that maps
//! a `runLLMTurn`/`runLLMTurnFork` site id to the rendered answer type.
//! `tidepool_runtime::compile_haskell` drops the extract tempdir before it
//! returns, so this module invokes `tidepool-extract` directly into a
//! kept-alive tempdir (mirroring `compile_haskell`'s own `Command`
//! construction) and reads all three outputs — `<target>.cbor`, `meta.cbor`,
//! `asks.json` — from it.
//!
//! This is deliberately NOT a fork of the runtime's caching compile: turns are
//! one-shot `M a` expressions, the sidecar is small, and the extract call is
//! the ~2s floor either way. Keeping it here means the harness owns the
//! sidecar contract end-to-end.
//!
//! [`compile_turns`] is the multi-target entry point (extract's `--targets`
//! mode, `haskell/app/Main.hs`'s `runMultiTargetClosed` — see
//! `plans/post-restart/extract-wave/boot/03-targets-prereq.md`): ONE extract
//! spawn compiles N named targets against a SHARED merged `meta.cbor` /
//! `DataConTable`, returning one [`CompiledTurn`] per target. [`compile_turn`]
//! is now a thin single-target wrapper over it, so every existing caller's
//! signature and on-disk contract are unchanged.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use serde::Deserialize;
use tidepool_repr::serial::{read_cbor, read_metadata};
use tidepool_repr::{CoreExpr, DataConTable};

use crate::timing;

/// Process-global count of `tidepool-extract` spawns paid by
/// [`compile_turn`] — the extract-wave `boot` item's done-criterion needs a
/// live receipt that the self-iterating harness's pre-model-call compile
/// count actually dropped (see `plans/post-restart/extract-wave/boot/00-spec.md`),
/// and this is the single spawn function every path the self-harness launch
/// reaches funnels through (the outer session's boot seed and its
/// `render`/`loop` compiles, and the answerer `Harness`'s own boot seed — see
/// `tidepool-harness/tests/acceptance_boot_compile_count.rs` for the traced
/// call chain). PROCESS-GLOBAL, not per-`Harness`/per-node: a test asserting
/// on it must run as its own test binary so no other test's compiles land on
/// the same count (nextest already gives one process per test binary).
static EXTRACT_SPAWNS: AtomicU64 = AtomicU64::new(0);

/// Number of `tidepool-extract` spawns [`compile_turn`] has paid in this
/// process so far. `Ordering::SeqCst` so a reader on another thread (the
/// acceptance test's snapshot, taken from inside a model-provider callback
/// running on a different thread than the compiling turn) is guaranteed to
/// see every increment a compiling thread has performed before this call.
pub fn extract_spawn_count() -> u64 {
    EXTRACT_SPAWNS.load(Ordering::SeqCst)
}

/// Reset the process-global spawn counter to zero. For test isolation within
/// a single test binary that drives more than one `compile_turn`-reaching
/// launch and wants each launch's count in isolation.
pub fn reset_extract_spawn_count() {
    EXTRACT_SPAWNS.store(0, Ordering::SeqCst);
}

/// One `asks.json` entry: a yield-site id and its rendered answer type.
#[derive(Debug, Clone, Deserialize)]
pub struct AskSite {
    pub site: u32,
    #[serde(rename = "type")]
    pub ty: String,
}

/// The `asks.json` sidecar as a site-id → rendered-type map. Empty when the
/// source has no `runLLMTurn`/`runLLMTurnFork` sites (the extract always
/// writes the file — loud absence beats a silent missing lookup).
#[derive(Debug, Clone, Default)]
pub struct AsksSidecar {
    by_site: HashMap<u32, String>,
}

impl AsksSidecar {
    /// Build a sidecar from `(site, type)` pairs — the shape
    /// `tidepool_runtime::session::CompiledTurn::asks` carries, decoded from
    /// `run_turn`'s `TurnOut` wire payload for a BIND/EXPR verdict.
    pub fn from_pairs(pairs: Vec<(u32, String)>) -> Self {
        AsksSidecar {
            by_site: pairs.into_iter().collect(),
        }
    }

    /// The rendered answer type for a yield-site id, if the site is known.
    pub fn type_of(&self, site: u32) -> Option<&str> {
        self.by_site.get(&site).map(String::as_str)
    }

    /// Number of recorded sites.
    pub fn len(&self) -> usize {
        self.by_site.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_site.is_empty()
    }
}

/// A compiled turn: the Core expression, its constructor table, and the
/// typed-yield sidecar.
pub struct CompiledTurn {
    pub expr: CoreExpr,
    pub table: DataConTable,
    pub asks: AsksSidecar,
}

#[derive(Debug, thiserror::Error)]
pub enum CompileError {
    #[error("failed to spawn tidepool-extract ({bin}): {source}")]
    Spawn { bin: String, source: std::io::Error },
    #[error("tidepool-extract failed:\n{0}")]
    Extract(String),
    #[error("io error during compile: {0}")]
    Io(#[from] std::io::Error),
    #[error("missing extract output: {0}")]
    MissingOutput(PathBuf),
    #[error("failed to deserialize extract output: {0}")]
    Deserialize(String),
    #[error("failed to parse asks.json: {0}")]
    Asks(String),
}

/// Compile `source` (a fully-templated Haskell module) with entry binder
/// `target`, searching `include` for modules, into `(expr, table, asks)`.
///
/// `extract_bin` is the `tidepool-extract` binary path (normally from
/// `TIDEPOOL_EXTRACT`); pass it explicitly so the harness resolves it once at
/// construction rather than re-reading the env per turn.
///
/// `node`/`round` attribute this compile's [`timing`] stages — pass
/// [`timing::NO_ROUND`] when the caller has no answerer-round context (only a
/// node id).
///
/// A thin wrapper over [`compile_turns`] (a one-element target slice) — the
/// multi-target `--targets` extract mode reduces to exactly this single-target
/// shape for N=1 (one plain `asks.json`, no cross-target meta merge to
/// perform), so every existing caller here gets byte-for-byte the same
/// on-disk contract it always has, now exercised through the shared
/// implementation instead of a separate one.
pub fn compile_turn(
    extract_bin: &str,
    source: &str,
    target: &str,
    include: &[PathBuf],
    node: u64,
    round: u64,
) -> Result<CompiledTurn, CompileError> {
    let mut turns = compile_turns(extract_bin, source, &[target], include, node, round)?;
    turns
        .remove(target)
        .ok_or_else(|| CompileError::MissingOutput(PathBuf::from(format!("{target}.cbor"))))
}

/// One target's raw (pre-deserialize) bytes, gathered by [`compile_turns`]
/// before the deserialize/asks-parse passes below.
struct RawTargetOutput {
    target: String,
    expr_bytes: Vec<u8>,
    asks_bytes: Option<Vec<u8>>,
}

/// Compile `source` against MULTIPLE named targets in ONE `tidepool-extract`
/// spawn: `targets.len()` `<target>.cbor` trees over a SINGLE shared merged
/// `meta.cbor` / [`DataConTable`], returning one [`CompiledTurn`] per target.
/// Mirrors [`compile_turn`]'s single-spawn contract but drives the extract's
/// `--targets a,b` mode (`haskell/app/Main.hs`'s `runMultiTargetClosed`)
/// instead of `--target` — see
/// `plans/post-restart/extract-wave/boot/03-targets-prereq.md`.
///
/// A REQUESTED target is a contract: the extract mode this drives fails the
/// WHOLE spawn (a nonzero exit, surfaced as [`CompileError::Extract`]) if
/// ANY target can't translate, rather than silently emitting the targets that
/// succeeded — this function adds no `try`/skip of its own on top of that,
/// so it inherits the same all-or-nothing guarantee.
///
/// asks sidecar shape: for exactly one target, the extract writes the plain
/// `asks.json` array — the single-target contract every extract build has
/// always produced (see [`compile_turn`]'s doc above). For more than one
/// target it additionally writes `<target>.asks.json` per target, so two
/// targets' DIFFERENT runLLMTurn/runLLMTurnFork sites never collapse into one
/// ambiguous file (`writeClosedTargets`'s doc comment, `Main.hs`) — this
/// function reads whichever shape the spawn actually produced, keyed on the
/// same `targets.len() > 1` test the Haskell side uses to decide which shape
/// to write.
///
/// `node`/`round` attribute this compile's [`timing`] stages exactly as
/// [`compile_turn`] does, now summed across every requested target instead of
/// just one.
pub fn compile_turns(
    extract_bin: &str,
    source: &str,
    targets: &[&str],
    include: &[PathBuf],
    node: u64,
    round: u64,
) -> Result<HashMap<String, CompiledTurn>, CompileError> {
    assert!(
        !targets.is_empty(),
        "compile_turns: at least one target is required"
    );
    let temp_dir = tempfile::TempDir::new()?;
    // GHC derives the module name from the filename (capitalize(basename)); the
    // templated preamble declares `module Expr`, so the file must be `Expr.hs`.
    let module = extract_module_name(source).unwrap_or_else(|| "Expr".to_string());
    let input_path = temp_dir.path().join(format!("{module}.hs"));
    std::fs::write(&input_path, source)?;

    let mut cmd = Command::new(extract_bin);
    cmd.arg(&input_path);
    cmd.arg("--output-dir").arg(temp_dir.path());
    cmd.arg("--targets").arg(targets.join(","));
    for path in include {
        cmd.arg("--include").arg(path);
    }
    let spawn_start = Instant::now();
    let output = cmd.output().map_err(|source| CompileError::Spawn {
        bin: extract_bin.to_string(),
        source,
    })?;
    // Counted on a successful spawn (the process actually launched and ran to
    // exit) — a `Spawn` error above (bad path, `Command::output` I/O failure)
    // never paid a real `tidepool-extract` cost and must not count as one.
    EXTRACT_SPAWNS.fetch_add(1, Ordering::Relaxed);
    timing::record_stage(
        node,
        round,
        timing::STAGE_EXTRACT_SPAWN,
        spawn_start.elapsed(),
        0,
    );
    // A failed compile is still a real answerer round — attribute its extract
    // phases the same as a successful one, before returning the error below.
    let extract_timing = timing::ExtractTiming::parse(&String::from_utf8_lossy(&output.stderr));
    timing::record_extract_phases(node, round, &extract_timing);
    if !output.status.success() {
        tracing::warn!(
            targets = %targets.join(","),
            "extract failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return Err(CompileError::Extract(format!(
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        )));
    }

    let meta_path = temp_dir.path().join("meta.cbor");
    if !meta_path.exists() {
        return Err(CompileError::MissingOutput(meta_path));
    }

    // A single requested target reads the plain `asks.json`; more than one
    // reads `<target>.asks.json` per target — see this fn's doc comment.
    let multi = targets.len() > 1;

    let cbor_read_start = Instant::now();
    let meta_bytes = std::fs::read(&meta_path)?;
    let mut raw: Vec<RawTargetOutput> = Vec::with_capacity(targets.len());
    for target in targets {
        let expr_path = temp_dir.path().join(format!("{target}.cbor"));
        if !expr_path.exists() {
            return Err(CompileError::MissingOutput(expr_path));
        }
        let expr_bytes = std::fs::read(&expr_path)?;
        let asks_path = if multi {
            temp_dir.path().join(format!("{target}.asks.json"))
        } else {
            temp_dir.path().join("asks.json")
        };
        let asks_bytes = read_asks_bytes(&asks_path)?;
        raw.push(RawTargetOutput {
            target: (*target).to_string(),
            expr_bytes,
            asks_bytes,
        });
    }
    let cbor_read_bytes = meta_bytes.len()
        + raw
            .iter()
            .map(|r| r.expr_bytes.len() + r.asks_bytes.as_ref().map_or(0, Vec::len))
            .sum::<usize>();
    timing::record_stage(
        node,
        round,
        timing::STAGE_CBOR_READ,
        cbor_read_start.elapsed(),
        cbor_read_bytes as u64,
    );

    let deserialize_start = Instant::now();
    let (table, warnings) =
        read_metadata(&meta_bytes).map_err(|e| CompileError::Deserialize(e.to_string()))?;
    let exprs: Vec<CoreExpr> = raw
        .iter()
        .map(|r| read_cbor(&r.expr_bytes).map_err(|e| CompileError::Deserialize(e.to_string())))
        .collect::<Result<Vec<_>, CompileError>>()?;
    timing::record_stage(
        node,
        round,
        timing::STAGE_CBOR_DESERIALIZE,
        deserialize_start.elapsed(),
        0,
    );
    // Register varId → name pairs so runtime unresolved-variable errors can name
    // the symbol (mirrors compile_haskell) — once, over the shared merged table.
    tidepool_codegen::host_fns::register_var_names(&warnings.var_names);

    let asks_start = Instant::now();
    let mut turns = HashMap::with_capacity(targets.len());
    for (r, expr) in raw.into_iter().zip(exprs.into_iter()) {
        let RawTargetOutput {
            target,
            expr_bytes,
            asks_bytes,
        } = r;
        let asks = parse_asks(asks_bytes)?;
        let mut sites: Vec<_> = asks.by_site.iter().collect();
        sites.sort_by_key(|(site, _)| **site);
        tracing::info!(
            target = %target,
            module = %module,
            expr_bytes = expr_bytes.len(),
            sites = ?sites,
            "compiled turn"
        );
        turns.insert(
            target,
            CompiledTurn {
                expr,
                table: table.clone(),
                asks,
            },
        );
    }
    timing::record_stage(
        node,
        round,
        timing::STAGE_ASKS_PARSE,
        asks_start.elapsed(),
        0,
    );

    Ok(turns)
}

/// Read the `asks.json` sidecar's raw bytes. A missing file yields `None`
/// (older extract without the pass) rather than an error; any other read
/// failure is a hard error.
fn read_asks_bytes(path: &Path) -> Result<Option<Vec<u8>>, CompileError> {
    match std::fs::read(path) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(CompileError::Io(e)),
    }
}

/// Parse the `asks.json` sidecar's bytes (if the extract wrote one) into a
/// sidecar. `None` (file absent) yields an empty sidecar; present-but-malformed
/// bytes are a hard error (a real regression to surface).
fn parse_asks(bytes: Option<Vec<u8>>) -> Result<AsksSidecar, CompileError> {
    let Some(bytes) = bytes else {
        return Ok(AsksSidecar::default());
    };
    let sites: Vec<AskSite> =
        serde_json::from_slice(&bytes).map_err(|e| CompileError::Asks(e.to_string()))?;
    Ok(AsksSidecar {
        by_site: sites.into_iter().map(|s| (s.site, s.ty)).collect(),
    })
}

/// Extract the module name from a `module <Name> where` header (GHC derives the
/// filename from it). Mirrors `tidepool_runtime`'s private helper.
fn extract_module_name(source: &str) -> Option<String> {
    for line in source.lines() {
        let line = line.trim_start();
        if let Some(rest) = line.strip_prefix("module ") {
            let name = rest.split_whitespace().next()?;
            return Some(name.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Forwarding path proof: a canned extract stderr (as `compile_turn` would
    /// see it with `TIDEPOOL_TIMING=1` set) parses into the phases
    /// `record_extract_phases` re-emits as `extract.*` stages — without ever
    /// shelling a real extract (the bench covers that).
    #[test]
    fn extract_stderr_timing_lines_forward_via_parse_and_record() {
        // The `ghc_session` line here is a historical fixture (this test
        // exercises the PARSER, which accepts any phase name) — a REAL
        // compile-lane stderr never emits `ghc_session` post-partition, only
        // `ghc_setup`/`ghc_load` (see `PHASE_GHC_SESSION`'s tombstone doc);
        // both new names are covered alongside it below.
        let stderr = "\
some ghc warning\n\
tidepool-timing phase=startup ms=12\n\
tidepool-timing phase=ghc_session ms=980\n\
tidepool-timing phase=ghc_setup ms=94\n\
tidepool-timing phase=ghc_load ms=886\n\
tidepool-timing phase=typecheck ms=340\n\
tidepool-timing phase=total ms=1500\n";
        let parsed = timing::ExtractTiming::parse(stderr);
        assert_eq!(
            parsed.phases,
            vec![
                (timing::PHASE_STARTUP.to_string(), 12),
                (timing::PHASE_GHC_SESSION.to_string(), 980),
                (timing::PHASE_GHC_SETUP.to_string(), 94),
                (timing::PHASE_GHC_LOAD.to_string(), 886),
                (timing::PHASE_TYPECHECK.to_string(), 340),
                (timing::PHASE_TOTAL.to_string(), 1500),
            ]
        );
        // Never an error path: an empty/absent-timing stderr forwards zero phases.
        assert!(timing::ExtractTiming::parse("plain ghc noise\n").is_empty());
        // The re-emit call site `compile_turn` uses on both the success and
        // failure paths — proves it accepts the parsed result without panicking.
        timing::record_extract_phases(timing::NO_NODE, timing::NO_ROUND, &parsed);
    }
}
