//! Multi-target `tidepool-extract` compilation: the mechanics shared by
//! [`crate::compile_haskell`] (one target, no sidecar) and [`compile_targets`]
//! (N targets sharing one GHC session, with the `asks.json` typed-yield
//! sidecar) — spawning the extractor, reading its output directory, and
//! deserializing into typed artifacts.
//!
//! Moved here from `tidepool_harness::compile` (architecture review finding
//! 3, 2026-08-17): that module was a second compiler frontend duplicating
//! this crate's temp-file setup, `ExtractCmd` construction, output-presence
//! checking, and CBOR reads, with its own parallel `CompileError` family.
//! This module is now the ONE place that does that work; the harness maps
//! [`CompiledArtifacts`] onto its own turn/node vocabulary
//! (`tidepool_harness::engine::compile_turn`/`compile_turns`) and attributes
//! timing to its own (node, round) pairs via the `on_stage` hook below,
//! rather than duplicating the spawn+read+deserialize sequence.
//!
//! # Two independent callers, two independent caches
//!
//! [`crate::compile_haskell`] and [`compile_targets`] share this module's
//! spawn/read/deserialize mechanics but keep their OWN, pre-existing cache
//! schemes: [`crate::compile_haskell`] still keys through
//! [`crate::cache::cache_key_salted`] / [`crate::cache::cache_load`] /
//! [`crate::cache::cache_store`] (a single `(expr, meta)` pair, unsalted by
//! default); [`compile_targets`] keys through
//! [`crate::cache::invocation_key`] / [`crate::cache::artifacts_load`] /
//! [`crate::cache::artifacts_store`] (a named artifact SET, which is what
//! lets the asks sidecar and multiple targets share one memo entry). Merging
//! those two key spaces was explicitly out of scope for the move: a key
//! change would cold every existing on-disk memo (including the harness test
//! suite's shared one), so each caller's cache wrapper is untouched — only
//! the code BETWEEN "cache miss" and "cache store" (the actual compiling) is
//! now shared.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Deserialize;
use tempfile::TempDir;
use tidepool_extract_cmd::{ExitVerdict, ExtractCmd, ResolvedExtractBin};
use tidepool_repr::serial::{read_cbor, read_metadata, MetaWarnings};
use tidepool_repr::{CoreExpr, DataConTable};

use crate::{cache, diag, extract_module_name, extract_spawn_error, timing, CompileError};

// ---------------------------------------------------------------------------
// The asks.json sidecar
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// The artifact bundle
// ---------------------------------------------------------------------------

/// One target's compiled output: the Core expression and its typed-yield
/// sidecar. The constructor table and warnings are SHARED across every
/// target in the same [`CompiledArtifacts`] (one GHC session, one merged
/// `meta.cbor`).
pub struct TargetArtifact {
    pub expr: CoreExpr,
    pub asks: AsksSidecar,
}

/// The full output of one `tidepool-extract` invocation: a shared constructor
/// table + warnings, and one [`TargetArtifact`] per requested target.
pub struct CompiledArtifacts {
    /// DataCon metadata the JIT needs to dispatch on constructors — shared by
    /// every target (they compiled in the same GHC session).
    pub table: DataConTable,
    /// Compile warnings (e.g. `has_io`, captured type) — shared by every
    /// target, same reason.
    pub warnings: MetaWarnings,
    /// Per-target Core + asks sidecar, keyed by target name.
    pub targets: BTreeMap<String, TargetArtifact>,
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Compile `source` against MULTIPLE named targets in ONE `tidepool-extract`
/// spawn: `targets.len()` `<target>.cbor` trees over a SINGLE shared merged
/// `meta.cbor` / [`DataConTable`], returning one [`TargetArtifact`] per
/// target inside a shared [`CompiledArtifacts`]. Drives the extract's
/// `--targets a,b` mode (`haskell/app/Main.hs`'s `runMultiTargetClosed`) —
/// see `plans/post-restart/extract-wave/boot/03-targets-prereq.md`.
///
/// A REQUESTED target is a contract: a nonzero exit fails the WHOLE spawn if
/// ANY target can't translate, rather than silently emitting the targets
/// that succeeded.
///
/// Asks sidecar shape: exactly one target writes the plain `asks.json`
/// array; more than one additionally writes `<target>.asks.json` per target,
/// so two targets' different `runLLMTurn`/`runLLMTurnFork` sites never
/// collapse into one ambiguous file.
///
/// `bin`: the resolved extract binary. `None` resolves fresh via
/// `$TIDEPOOL_EXTRACT`/`PATH` (`ExtractCmd::new`, [`crate::compile_haskell`]'s
/// behavior); `Some` is for a caller that resolves once at construction and
/// threads the binary through many calls rather than re-reading the env
/// every time (`tidepool_harness::engine::EngineConfig`).
///
/// **MEMOIZED** through [`cache::invocation_key`] — see the module doc for
/// why this is a separate scheme from [`crate::compile_haskell`]'s.
///
/// `on_stage(name, elapsed, bytes)` fires once per measured stage —
/// [`timing::STAGE_EXTRACT_SPAWN`], each forwarded `extract.<phase>` row
/// parsed from the extract's stderr, [`timing::STAGE_CBOR_READ`],
/// [`timing::STAGE_CBOR_DESERIALIZE`], [`timing::STAGE_ASKS_PARSE`] — so a
/// caller with its own attribution vocabulary (node/round ids) can record
/// through its own collector without this crate needing to know what a
/// "node" or "round" is. A caller with no use for timing passes `|_, _, _|
/// {}`.
pub fn compile_targets(
    source: &str,
    targets: &[&str],
    include: &[PathBuf],
    bin: Option<&ResolvedExtractBin>,
    mut on_stage: impl FnMut(&str, Duration, u64),
) -> Result<CompiledArtifacts, CompileError> {
    assert!(
        !targets.is_empty(),
        "compile_targets: at least one target is required"
    );
    let temp_dir = TempDir::new()?;
    // GHC derives the module name from the filename (capitalize(basename));
    // the templated preamble declares `module Expr`, so the file must be
    // `Expr.hs` when the source has no explicit `module` header of its own.
    let module = extract_module_name(source).unwrap_or_else(|| "Expr".to_string());
    let input_path = temp_dir.path().join(format!("{module}.hs"));
    std::fs::write(&input_path, source)?;

    // A single requested target reads the plain `asks.json`; more than one
    // reads `<target>.asks.json` per target — see this fn's doc comment.
    let multi = targets.len() > 1;

    let mut cmd = match bin {
        Some(b) => ExtractCmd::with_bin(b.clone()),
        None => ExtractCmd::new().map_err(|e| CompileError::Io(e.into()))?,
    };
    cmd.input(&input_path)
        .output_dir(temp_dir.path())
        .targets(targets)
        .includes(include);

    // Keyed on the invocation that is about to run — the built argv itself,
    // so a flag this site grows cannot ride along unkeyed (the allowlist
    // walk in `invocation_key` makes an unclassified flag uncacheable rather
    // than silently unkeyed). `None` means "compile cold", never "compile
    // wrong".
    let argv = cmd.argv();
    let bin_path: PathBuf = match bin {
        Some(b) => b.as_path().to_path_buf(),
        None => PathBuf::from(tidepool_extract_cmd::DEFAULT_BIN),
    };
    let key = cache::invocation_key(&cache::Invocation {
        source,
        argv: &argv,
        input_path: &input_path,
        include,
        bin: &bin_path,
    });
    let names = artifact_names(targets, multi);
    let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();

    if let Some(key) = &key {
        let load_start = Instant::now();
        if let Some((meta_bytes, raw)) = load_memo(key, &name_refs, targets) {
            let bytes = total_bytes(&meta_bytes, &raw);
            on_stage(timing::STAGE_CBOR_READ, load_start.elapsed(), bytes);
            return assemble(&meta_bytes, &raw, on_stage);
        }
    }

    let (meta_bytes, raw) = extract_and_read(
        &cmd,
        temp_dir.path(),
        targets,
        multi,
        &mut on_stage,
        |stderr, success| {
            if !success && !stderr.is_empty() {
                tracing::warn!(
                    targets = %targets.join(","),
                    "extract failed:\n{stderr}"
                );
            }
        },
    )?;

    // Store only what DESERIALIZED, so a malformed artifact set is never
    // memoized into a permanently-failing entry. Best-effort: an unwritable
    // memo costs a recompile, it never fails a compile.
    let artifacts = assemble(&meta_bytes, &raw, &mut on_stage)?;
    if let Some(key) = &key {
        store_memo(key, &name_refs, &meta_bytes, &raw);
    }
    Ok(artifacts)
}

// ---------------------------------------------------------------------------
// Shared spawn + read + deserialize (also used by `crate::compile_haskell`)
// ---------------------------------------------------------------------------

/// One target's raw (pre-deserialize) bytes.
pub(crate) struct RawTargetOutput {
    pub(crate) target: String,
    pub(crate) expr_bytes: Vec<u8>,
    asks_bytes: Option<Vec<u8>>,
}

/// Spawn `cmd` (already fully configured — input, output-dir, target(s),
/// includes) and pull `targets`' bytes off `temp_dir`, forwarding every
/// measured stage through `on_stage`. `log_stderr(text, success)` is called
/// once after the spawn, whatever the outcome, so each caller can apply its
/// own logging policy (`compile_haskell` always echoes non-empty stderr;
/// `compile_targets` only warns on failure).
///
/// A nonzero exit is read through the structured diagnostics contract
/// ([`diag::parse_diag_report`]) — the SAME reading [`crate::compile_haskell`]
/// already gives an ordinary eval compile, so a bad target name or any other
/// GHC-detectable failure here reports real spans, not an opaque stdout/stderr
/// dump.
pub(crate) fn extract_and_read(
    cmd: &ExtractCmd,
    temp_dir: &Path,
    targets: &[&str],
    multi: bool,
    mut on_stage: impl FnMut(&str, Duration, u64),
    log_stderr: impl FnOnce(&str, bool),
) -> Result<(Vec<u8>, Vec<RawTargetOutput>), CompileError> {
    let run = cmd
        .run()
        .map_err(|e| CompileError::Io(extract_spawn_error(e.source)))?;
    on_stage(timing::STAGE_EXTRACT_SPAWN, run.elapsed, 0);

    let stderr = run.stderr_lossy();
    let extract_timing = timing::ExtractTiming::parse(&stderr);
    for (phase, ms) in &extract_timing.phases {
        on_stage(
            &timing::extract_stage_name(phase),
            Duration::from_millis(*ms),
            0,
        );
    }
    log_stderr(&stderr, run.verdict == ExitVerdict::Success);

    if run.verdict != ExitVerdict::Success {
        return Err(
            match diag::parse_diag_report(&run.output.stdout, &run.output.stderr) {
                Ok(report) => CompileError::Diagnostics(report.diagnostics),
                Err(msg) => CompileError::MalformedDiagnostics(msg),
            },
        );
    }

    let cbor_read_start = Instant::now();
    let meta_path = temp_dir.join("meta.cbor");
    if !meta_path.exists() {
        return Err(CompileError::MissingOutput(meta_path));
    }
    let meta_bytes = std::fs::read(&meta_path)?;

    let mut raw = Vec::with_capacity(targets.len());
    for target in targets {
        let expr_path = temp_dir.join(format!("{target}.cbor"));
        if !expr_path.exists() {
            return Err(CompileError::MissingOutput(expr_path));
        }
        let expr_bytes = std::fs::read(&expr_path)?;
        let asks_path = if multi {
            temp_dir.join(format!("{target}.asks.json"))
        } else {
            temp_dir.join("asks.json")
        };
        let asks_bytes = read_asks_bytes(&asks_path)?;
        raw.push(RawTargetOutput {
            target: (*target).to_string(),
            expr_bytes,
            asks_bytes,
        });
    }
    on_stage(
        timing::STAGE_CBOR_READ,
        cbor_read_start.elapsed(),
        total_bytes(&meta_bytes, &raw),
    );
    Ok((meta_bytes, raw))
}

/// Deserialize a `(meta_bytes, raw)` pair — from a fresh spawn or a memo hit
/// — into a [`CompiledArtifacts`]. Both paths landing here (rather than each
/// deserializing separately) is what makes a cache hit observationally
/// identical to a cold compile.
pub(crate) fn assemble(
    meta_bytes: &[u8],
    raw: &[RawTargetOutput],
    mut on_stage: impl FnMut(&str, Duration, u64),
) -> Result<CompiledArtifacts, CompileError> {
    let deserialize_start = Instant::now();
    let (table, warnings) = read_metadata(meta_bytes)?;
    let exprs: Vec<CoreExpr> = raw
        .iter()
        .map(|r| read_cbor(&r.expr_bytes).map_err(CompileError::from))
        .collect::<Result<Vec<_>, CompileError>>()?;
    on_stage(
        timing::STAGE_CBOR_DESERIALIZE,
        deserialize_start.elapsed(),
        0,
    );
    // Register varId → name pairs so runtime unresolved-variable errors can
    // name the symbol, and sentinel-slot → external-name pairs so a forced
    // kind-4 poison names the symbol it replaced — once, over the shared
    // merged table.
    tidepool_codegen::host_fns::register_var_names(&warnings.var_names);
    tidepool_codegen::host_fns::register_poisoned_externals(&warnings.poisoned);

    let asks_start = Instant::now();
    let mut targets = BTreeMap::new();
    for (r, expr) in raw.iter().zip(exprs.into_iter()) {
        let asks = parse_asks(r.asks_bytes.as_deref())?;
        targets.insert(r.target.clone(), TargetArtifact { expr, asks });
    }
    on_stage(timing::STAGE_ASKS_PARSE, asks_start.elapsed(), 0);

    Ok(CompiledArtifacts {
        table,
        warnings,
        targets,
    })
}

/// Read the `asks.json` sidecar's raw bytes. A missing file yields `None`
/// (older extract without the pass, or a target that made no
/// `runLLMTurn`/`runLLMTurnFork` calls) rather than an error; any other read
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
fn parse_asks(bytes: Option<&[u8]>) -> Result<AsksSidecar, CompileError> {
    let Some(bytes) = bytes else {
        return Ok(AsksSidecar::default());
    };
    let sites: Vec<AskSite> =
        serde_json::from_slice(bytes).map_err(|e| CompileError::Asks(e.to_string()))?;
    Ok(AsksSidecar {
        by_site: sites.into_iter().map(|s| (s.site, s.ty)).collect(),
    })
}

// ---------------------------------------------------------------------------
// Invocation-keyed memo glue (compile_targets only — see the module doc)
// ---------------------------------------------------------------------------

/// The logical artifact names of one invocation's output set — exactly the
/// filenames [`compile_targets`] reads out of the extract's output dir, in a
/// fixed order: the shared `meta.cbor`, then per target its `<target>.cbor`
/// and its asks sidecar. `multi` picks the sidecar SHAPE on the same
/// `targets.len() > 1` test the Haskell side uses to decide which shape to
/// write, so the memo's names track the extract's own contract.
fn artifact_names(targets: &[&str], multi: bool) -> Vec<String> {
    let mut names = Vec::with_capacity(1 + targets.len() * 2);
    names.push("meta.cbor".to_string());
    for target in targets {
        names.push(format!("{target}.cbor"));
        names.push(if multi {
            format!("{target}.asks.json")
        } else {
            "asks.json".to_string()
        });
    }
    names
}

/// Total bytes read for this compile, for the `cbor_read` timing stage —
/// identical on the memo path and the spawn path, since both count the same
/// artifacts.
fn total_bytes(meta_bytes: &[u8], raw: &[RawTargetOutput]) -> u64 {
    let total = meta_bytes.len()
        + raw
            .iter()
            .map(|r| r.expr_bytes.len() + r.asks_bytes.as_ref().map_or(0, Vec::len))
            .sum::<usize>();
    total as u64
}

/// Reassemble a memoized artifact set into the same `(meta, raw)` pair the
/// spawn path produces. `None` — any absent-but-required artifact, or a set
/// the memo declines — falls through to a cold compile.
fn load_memo(
    key: &cache::InvocationKey,
    names: &[&str],
    targets: &[&str],
) -> Option<(Vec<u8>, Vec<RawTargetOutput>)> {
    let loaded = cache::artifacts_load(key, names)?;
    let mut it = loaded.into_iter();
    // `meta.cbor` and every `<target>.cbor` are required (the spawn path errors
    // with `MissingOutput` without them); the asks sidecar is legitimately
    // absent for an extract predating that pass, and `None` must survive as
    // `None` so `parse_asks` yields an empty sidecar rather than parsing `[]`.
    let meta_bytes = it.next()??;
    let mut raw = Vec::with_capacity(targets.len());
    for target in targets {
        let expr_bytes = it.next()??;
        let asks_bytes = it.next()?;
        raw.push(RawTargetOutput {
            target: (*target).to_string(),
            expr_bytes,
            asks_bytes,
        });
    }
    Some((meta_bytes, raw))
}

/// Store this invocation's full artifact set under `names`, in the order
/// [`artifact_names`] fixed.
fn store_memo(
    key: &cache::InvocationKey,
    names: &[&str],
    meta_bytes: &[u8],
    raw: &[RawTargetOutput],
) {
    let mut artifacts: Vec<(&str, Option<&[u8]>)> = Vec::with_capacity(names.len());
    let mut names = names.iter();
    if let Some(name) = names.next() {
        artifacts.push((name, Some(meta_bytes)));
    }
    for r in raw {
        let (Some(expr_name), Some(asks_name)) = (names.next(), names.next()) else {
            return;
        };
        artifacts.push((expr_name, Some(r.expr_bytes.as_slice())));
        artifacts.push((asks_name, r.asks_bytes.as_deref()));
    }
    cache::artifacts_store(key, &artifacts);
}
