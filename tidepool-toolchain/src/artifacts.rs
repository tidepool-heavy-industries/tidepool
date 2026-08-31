//! The ONE policy-bearing `tidepool-extract` compile front door:
//! [`CompileInvocation`] + [`compile_invocation`]. `tidepool_runtime::compile_haskell`
//! (one target, eval/session lane) and [`compile_targets`] (N targets sharing
//! one GHC session, harness turn lane) are both thin projections that build a
//! [`CompileInvocation`] and hand it to [`compile_invocation`] — spawning the
//! extractor, reading its output directory, and deserializing into typed
//! artifacts happens in exactly one place. The harness maps
//! [`CompiledArtifacts`] onto its own turn/node vocabulary
//! (`tidepool_harness::engine::compile_turn`/`compile_turns`) and attributes
//! timing to its own (node, round) pairs via the `on_stage` hook, rather than
//! duplicating the spawn+read+deserialize sequence.
//!
//! # One front door, two DELIBERATELY separate cache schemes
//!
//! [`CompileInvocation::cache`] ([`CacheStrategy`]) is the one remaining
//! policy delta between the lanes. The eval lane
//! (`tidepool_runtime::compile_haskell`/`tidepool_runtime::compile_haskell_salted`) keys
//! through [`crate::cache::eval_cache_key`] / [`crate::cache::cache_load`] /
//! [`crate::cache::cache_store`] (a single `(expr, meta)` pair, optionally
//! salted per session/generation); the turn lane ([`compile_targets`]) keys
//! through [`crate::cache::invocation_key`] / [`crate::cache::artifacts_load`]
//! / [`crate::cache::artifacts_store`] (a named artifact SET, which is what
//! lets the asks sidecar and multiple targets share one memo entry, but has
//! no salt concept). Merging those two key spaces is out of scope: a key
//! change would cold every existing on-disk memo (including the harness test
//! suite's shared one and every deployed eval cache) for whichever lane's
//! scheme lost.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Deserialize;
use tempfile::TempDir;
use tidepool_extract_cmd::{ExtractCmd, ResolvedExtractBin};
use tidepool_repr::serial::{read_cbor, read_metadata, MetaWarnings};
use tidepool_repr::{CoreExpr, DataConTable};

use crate::{cache, diag, extract_module_name, extract_spawn_error, timing, CompileError};

// ---------------------------------------------------------------------------
// Typed yield sites (`asks.json` on disk)
// ---------------------------------------------------------------------------

/// One compiler sidecar entry: a typed suspension-site id, its rendered answer
/// type, any live input types, and the defining modules needed to resolve each
/// type by name —
/// `Tidepool.Translate.modulesOfType`'s result for every tycon the type
/// mentions (the type's own head plus every type argument's head). Extract
/// has the type environment in hand at the call site, so it reports this
/// directly; `modules` is NOT `#[serde(default)]` — an extract binary old
/// enough not to emit it fails this deserialization loudly (`CompileError::
/// Asks`) rather than silently resolving with no modules, since a caller
/// that built an `AnswerContract` from an empty list would compile a shim
/// that cannot name the type at all.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct YieldSite {
    pub site: u64,
    pub origin: String,
    pub ordinal: u64,
    #[serde(rename = "type")]
    pub ty: String,
    pub modules: Vec<String>,
    pub heads: Vec<NominalHead>,
    pub inputs: Vec<SiteType>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NominalHead {
    pub unit: String,
    pub module: String,
    pub name: String,
}

/// One GHC-rendered live input type attached to a suspension site.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct SiteType {
    #[serde(rename = "type")]
    pub ty: String,
    pub modules: Vec<String>,
    pub heads: Vec<NominalHead>,
}

/// The typed-suspension sidecar indexed by site id. The artifact retains its
/// historical `asks.json` filename for now, but its in-memory contract is no
/// longer ask-specific.
#[derive(Debug, Clone, Default)]
pub struct YieldSites {
    by_site: HashMap<u64, YieldSite>,
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
#[error("typed-site collision at {site}: {first:?} != {second:?}")]
pub struct YieldSiteCollision {
    pub site: u64,
    pub first: Box<YieldSite>,
    pub second: Box<YieldSite>,
}

impl YieldSites {
    /// Test convenience for answer-only sites with no module information.
    pub fn from_pairs(pairs: Vec<(u64, String)>) -> Self {
        YieldSites {
            by_site: pairs
                .into_iter()
                .map(|(site, ty)| {
                    (
                        site,
                        YieldSite {
                            site,
                            origin: "<test>".into(),
                            ordinal: site,
                            ty,
                            modules: Vec::new(),
                            heads: Vec::new(),
                            inputs: Vec::new(),
                        },
                    )
                })
                .collect(),
        }
    }

    /// Build an answer-only lookup from `(site, type, modules)` triples, for tests
    /// that need output-type module resolution but no live inputs.
    pub fn from_entries(entries: Vec<(u64, String, Vec<String>)>) -> Self {
        YieldSites {
            by_site: entries
                .into_iter()
                .map(|(site, ty, modules)| {
                    (
                        site,
                        YieldSite {
                            site,
                            origin: "<test>".into(),
                            ordinal: site,
                            ty,
                            modules,
                            heads: Vec::new(),
                            inputs: Vec::new(),
                        },
                    )
                })
                .collect(),
        }
    }

    /// Build a lookup from the compiler's complete typed-site records.
    pub fn from_sites(sites: Vec<YieldSite>) -> Result<Self, YieldSiteCollision> {
        let mut by_site: HashMap<u64, YieldSite> = HashMap::new();
        for site in sites {
            match by_site.get(&site.site) {
                Some(previous) if previous != &site => {
                    return Err(YieldSiteCollision {
                        site: site.site,
                        first: Box::new(previous.clone()),
                        second: Box::new(site),
                    });
                }
                _ => {
                    by_site.insert(site.site, site);
                }
            }
        }
        Ok(Self { by_site })
    }

    /// The rendered answer type for a yield-site id, if the site is known.
    pub fn type_of(&self, site: u64) -> Option<&str> {
        self.by_site.get(&site).map(|entry| entry.ty.as_str())
    }

    /// The defining modules a shim must import to resolve `site`'s answer
    /// type by name — empty when the site is unknown or the extract that
    /// produced this sidecar recorded no modules (e.g. a `Prelude`-only
    /// type).
    pub fn modules_of(&self, site: u64) -> &[String] {
        self.by_site
            .get(&site)
            .map(|entry| entry.modules.as_slice())
            .unwrap_or(&[])
    }

    /// GHC-derived live input types attached to this suspension site.
    pub fn inputs_of(&self, site: u64) -> &[SiteType] {
        self.by_site
            .get(&site)
            .map(|entry| entry.inputs.as_slice())
            .unwrap_or(&[])
    }

    /// Every recorded answer `(site, type, modules)` entry, in no particular
    /// order. Live-input metadata remains available through [`Self::inputs_of`].
    pub fn iter(&self) -> impl Iterator<Item = (u64, &str, &[String])> {
        self.by_site
            .iter()
            .map(|(site, entry)| (*site, entry.ty.as_str(), entry.modules.as_slice()))
    }

    /// Complete compiler records in stable site-id order. Use this when a
    /// compiled program crosses into a provenance-owning runtime API; compact
    /// lookup consumers should continue to use [`Self::iter`].
    #[must_use]
    pub fn sites(&self) -> Vec<YieldSite> {
        let mut sites: Vec<_> = self.by_site.values().cloned().collect();
        sites.sort_by_key(|site| site.site);
        sites
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
// Stable session-Val injection (turn-latency-state-injection)
// ---------------------------------------------------------------------------

/// A stable, never-rotating session `Val` module to inject via
/// `--session-root <dir> --inject-val <module>` — see
/// [`cache::Invocation::stable_val`]'s doc for why this is safe to treat as
/// CACHEABLE despite naming a session-scoped iface. The ONLY caller today is
/// the self-iterating harness driver's fused outer render/loop compile;
/// every other `--inject-val`/`--session-root` use (the interactive session's rotating
/// `Val.G<g>` value plane, `tidepool_runtime::session::turn`) stays on the
/// ordinary uncacheable path and never constructs one of these.
pub struct StableValInject<'a> {
    pub module: tidepool_repr::SessionModule,
    pub session_root: &'a Path,
}

/// A session's `--session-root <dir> --inject-val <module>` (repeated) for
/// an UNCACHED probe compile — the plural sibling of [`StableValInject`],
/// which stays single-module and CACHEABLE (its own doc explains why that
/// invariant matters for its one caller). This one is for a caller that
/// needs to inject however many `Val.G<g>` modules are LIVE on a real,
/// mutable session at an arbitrary point — [`tidepool_harness::engine`]'s
/// pinned-`Finalize <T>`-row probe, when `T` is declared on the session's own
/// decl plane: `PersistentSession::define_scoped_in` splices an import of
/// every live `Val.G<g>` name into the generated decl module UNCONDITIONALLY
/// (the decl-plane analogue of GHCi seeing earlier bindings), so a decl
/// module that itself imports nothing from the value plane still names those
/// modules in its own source and needs them resolvable to compile at all.
/// Deliberately routed through [`CacheStrategy::Uncached`]
/// ([`compile_targets_with_session_inject`]) rather than widening
/// [`cache::Invocation::stable_val`] to a list: the live set varies with
/// session state in a way the compile memo's argv allowlist was never built
/// to key on, and the caller already has its OWN process-level memo over the
/// generated probe source (`tidepool_harness::engine`'s `finalize_probe_memo`).
pub struct SessionInject<'a> {
    pub session_root: &'a Path,
    pub inject_modules: &'a [String],
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
    pub asks: YieldSites,
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
// The one front door
// ---------------------------------------------------------------------------

/// How a [`CompileInvocation`]'s result is memoized — the one deliberate
/// policy delta between the two lanes; see the module doc.
pub enum CacheStrategy<'a> {
    /// `tidepool_runtime::compile_haskell`/`tidepool_runtime::compile_haskell_salted`'s scheme:
    /// a single `(expr, meta)` pair keyed by [`cache::eval_cache_key`].
    Eval { salt: Option<&'a str> },
    /// [`compile_targets`]'s scheme: a whole artifact SET keyed by
    /// [`cache::invocation_key`] over the built argv.
    Invocation,
    /// Never memoized, in either direction (no load, no store) — for a
    /// [`SessionInject`]ed compile, whose `--inject-val` set names a real
    /// session's live, mutable state rather than anything the compile memo's
    /// argv allowlist can key on. [`compile_targets_with_session_inject`]'s
    /// one caller already has its own memo over the compile it's probing.
    Uncached,
}

/// One `tidepool-extract` invocation, as built by either production front
/// door. `tidepool_runtime::compile_haskell_salted` builds one with a single-element
/// `targets` and [`CacheStrategy::Eval`]; [`compile_targets`] builds one with
/// N targets and [`CacheStrategy::Invocation`] — single-target compilation is
/// a PROJECTION of the same [`compile_invocation`] this drives for the batch
/// case, not a separate spawn/read/deserialize path.
pub struct CompileInvocation<'a> {
    pub source: &'a str,
    pub targets: &'a [&'a str],
    pub include: &'a [PathBuf],
    /// `None` resolves fresh via `$TIDEPOOL_EXTRACT`/`PATH`
    /// (`ExtractCmd::new`); `Some` is for a caller that resolved once at
    /// construction and threads the binary through many calls
    /// (`tidepool_harness::engine::EngineConfig`).
    pub bin: Option<&'a ResolvedExtractBin>,
    /// Fallback module name (sans `.hs`) when `source` has no `module`
    /// header — GHC derives the module name from the filename
    /// (`capitalize(basename)`), and the two lanes' templated preambles
    /// disagree on what that name must be: the turn lane's wrapper declares
    /// `module Expr`, the eval lane's historical default is `Input`.
    pub fallback_module_name: &'a str,
    pub cache: CacheStrategy<'a>,
    /// A [`StableValInject`] to apply to this invocation's `ExtractCmd`
    /// (`--session-root`/`--inject-val`), and to carry into the memo key as
    /// [`cache::Invocation::stable_val`] so the invocation stays cacheable.
    /// `None` for both existing front doors (`compile_haskell`,
    /// `compile_targets`) — only [`compile_targets_with_stable_inject`] sets
    /// it.
    pub stable_val: Option<StableValInject<'a>>,
    /// A [`SessionInject`] to apply to this invocation's `ExtractCmd`
    /// (`--session-root`/`--inject-val` per module) — mutually exclusive
    /// with `stable_val` in practice (no caller sets both). `None` for every
    /// front door except [`compile_targets_with_session_inject`]. Always
    /// paired with [`CacheStrategy::Uncached`] by that front door — see
    /// [`SessionInject`]'s doc for why.
    pub session_inject: Option<SessionInject<'a>>,
}

/// Compile a [`CompileInvocation`] against ONE `tidepool-extract` spawn:
/// `targets.len()` `<target>.cbor` trees over a SINGLE shared merged
/// `meta.cbor` / [`DataConTable`], returning one [`TargetArtifact`] per
/// target inside a shared [`CompiledArtifacts`]. Drives the extract's
/// `--targets a,b` mode (`haskell/app/Main.hs`'s `runMultiTargetClosed`,
/// which handles a single-element list identically to the legacy `--target`
/// flag).
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
/// **MEMOIZED** per [`CompileInvocation::cache`] — see the module doc for why
/// the two `CacheStrategy` variants stay separate schemes.
///
/// `on_stage(name, elapsed, bytes)` fires once per measured stage —
/// [`timing::STAGE_EXTRACT_SPAWN`], each forwarded `extract.<phase>` row
/// parsed from the extract's stderr, [`timing::STAGE_CBOR_READ`],
/// [`timing::STAGE_CBOR_DESERIALIZE`], [`timing::STAGE_ASKS_PARSE`] — so a
/// caller with its own attribution vocabulary (node/round ids) can record
/// through its own collector without this crate needing to know what a
/// "node" or "round" is. A caller with no use for timing passes `|_, _, _|
/// {}`.
pub fn compile_invocation(
    inv: &CompileInvocation<'_>,
    mut on_stage: impl FnMut(&str, Duration, u64),
) -> Result<CompiledArtifacts, CompileError> {
    assert!(
        !inv.targets.is_empty(),
        "compile_invocation: at least one target is required"
    );
    let multi = inv.targets.len() > 1;

    // The eval key needs no built argv (unlike invocation keying, it never
    // looks at what gets spawned), so it is computed and checked up front —
    // the same ordering `compile_haskell` always used, and it is reused
    // below for the cache-store call after a real compile.
    let eval_key = match &inv.cache {
        CacheStrategy::Eval { salt } => {
            let key = cache::eval_cache_key(inv.source, inv.targets[0], inv.include, *salt);
            if let Some(key) = &key {
                if let Some((expr_bytes, meta_bytes, asks_bytes)) = cache::cache_load(key) {
                    // Attempt to deserialize cached data. If this fails, treat it as
                    // a cache miss and fall through to recompilation instead of
                    // propagating the error.
                    let raw = vec![RawTargetOutput {
                        target: inv.targets[0].to_string(),
                        expr_bytes,
                        asks_bytes,
                    }];
                    if let Ok(artifacts) = assemble(&meta_bytes, &raw, &mut on_stage) {
                        return Ok(artifacts);
                    }
                }
            }
            key
        }
        _ => None,
    };

    let temp_dir = TempDir::new()?;
    // GHC derives the module name from the filename (capitalize(basename));
    // see `CompileInvocation::fallback_module_name`'s doc for why this
    // differs per lane.
    let module =
        extract_module_name(inv.source).unwrap_or_else(|| inv.fallback_module_name.to_string());
    let input_path = temp_dir.path().join(format!("{module}.hs"));
    std::fs::write(&input_path, inv.source)?;

    let mut cmd = match inv.bin {
        Some(b) => ExtractCmd::with_bin(b.clone()),
        None => ExtractCmd::new().map_err(|e| CompileError::Io(e.into()))?,
    };
    cmd.input(&input_path)
        .output_dir(temp_dir.path())
        .targets(inv.targets)
        .includes(inv.include);
    if let Some(sv) = &inv.stable_val {
        cmd.session_root(sv.session_root)
            .inject_val(sv.module.module_name());
    }
    if let Some(si) = &inv.session_inject {
        cmd.session_root(si.session_root)
            .inject_vals(si.inject_modules.iter().cloned());
    }

    // Persistent build-products dir (module-granular GHC recompilation
    // avoidance across spawns — see `crate::paths::build_products_dir`'s
    // doc). ON BY DEFAULT: turning this on used to change the compiled BYTES
    // for any turn/eval whose Core contains a nested (non-top-level) binder
    // (spike-verified, 2026-08-20 — cold-vs-cold was byte-identical, but a
    // cold-then-warm pair was NOT, because GHC's session-wide Unique-
    // allocation trajectory shifts when `load'` skips a variable number of
    // modules, and `Translate.hs`'s `localVarId` baked that raw Unique into
    // every nested Id's VarId). That gap is closed —
    // `Tidepool.Translate.stabilizeLocalUniques` (nested Ids) together with
    // `Tidepool.GhcPipeline.externalizeInternalTops`'s ordinal disambiguator
    // (internalized top-level floats) make a compile's VarIds a pure
    // function of Core shape, never of session Unique-allocation history —
    // see `tidepool-runtime/tests/build_products_dir_differential.rs`, the
    // byte-identical-cold-vs-warm acceptance gate for this mechanism.
    //
    // `crate::paths::build_products_dir` is keyed by the resolved extract
    // binary's own content fingerprint, so a rebuilt/updated extract gets a
    // FRESH directory — a stale dir from an older (pre-fix, or otherwise
    // different) extract binary can never poison a compile; staleness is
    // structurally impossible rather than mtime-validated. Known,
    // accepted characteristic (not newly introduced by this default-on
    // flip): the directory is SHARED across every concurrent spawn using the
    // same extract binary, so two truly concurrent compiles of DIFFERENT
    // source under the same module name (e.g. the turn lane's fixed
    // `Expr`/eval lane's fixed `Input`) race on the same `.hi`/`.o` path;
    // GHC's own interface content-hash check means the losing race forces a
    // recompile rather than silently reusing mismatched output, so the
    // failure mode is wasted work, not wrong output — see
    // the resident compile daemon, where a single worker process (not
    // many concurrent spawns) is the long-term answer.
    //
    // `$TIDEPOOL_BUILD_PRODUCTS_DIR` still overrides the LOCATION (an
    // isolated dir for a test that needs a genuinely cold measurement,
    // mirroring `compile_cache_dir`'s own override) — it is no longer also
    // the enable switch. Applied via `crate::paths::apply_build_products_dir`
    // — the same helper `session/turn.rs`'s `extract_cmd()` and
    // `session/mod.rs`'s `validate_candidate` call for their OWN spawn sites,
    // so this is on by default everywhere in this crate, not just here.
    crate::paths::apply_build_products_dir(&mut cmd);

    let names = artifact_names(inv.targets, multi);
    let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();

    // Keyed on the invocation that is about to run — the built argv itself,
    // so a flag this site grows cannot ride along unkeyed (the allowlist
    // walk in `invocation_key` makes an unclassified flag uncacheable rather
    // than silently unkeyed). `None` means "compile cold", never "compile
    // wrong".
    let inv_key = if matches!(inv.cache, CacheStrategy::Invocation) {
        let argv = cmd.argv();
        let bin_path: PathBuf = match inv.bin {
            Some(b) => b.as_path().to_path_buf(),
            None => PathBuf::from(tidepool_extract_cmd::DEFAULT_BIN),
        };
        let key = cache::invocation_key(&cache::Invocation {
            source: inv.source,
            argv: &argv,
            input_path: &input_path,
            include: inv.include,
            bin: &bin_path,
            stable_val: inv.stable_val.as_ref().map(|sv| sv.module),
        });
        if let Some(key) = &key {
            let load_start = Instant::now();
            if let Some((meta_bytes, raw)) = load_memo(key, &name_refs, inv.targets) {
                let bytes = total_bytes(&meta_bytes, &raw);
                on_stage(timing::STAGE_CBOR_READ, load_start.elapsed(), bytes);
                return assemble(&meta_bytes, &raw, on_stage);
            }
        }
        key
    } else {
        None
    };

    // The full spawn argv, rendered once: DEBUG on every spawn, and attached
    // to the failure WARN below. This is the record of what include set /
    // session-root / flags an individual spawn actually received — without
    // it, a spawn-specific resolution failure (a module present on disk that
    // one compile couldn't find, sprint-25 crash class) leaves nothing to
    // diff against a succeeding sibling invocation.
    let argv_render: String = cmd
        .argv()
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ");
    tracing::debug!(
        targets = %inv.targets.join(","),
        argv = %argv_render,
        "extract spawn"
    );

    let is_invocation_lane = matches!(inv.cache, CacheStrategy::Invocation);
    let (meta_bytes, raw) = extract_and_read(
        &cmd,
        temp_dir.path(),
        inv.targets,
        multi,
        &mut on_stage,
        |stderr, success| {
            // The invocation (turn) lane only warns on failure; the eval
            // lane always echoes stderr as a human debug channel — the same
            // logging policy each standalone function uses
            // this front door existed.
            if is_invocation_lane {
                if !success && !stderr.is_empty() {
                    tracing::warn!(
                        targets = %inv.targets.join(","),
                        "extract failed:\n{stderr}"
                    );
                }
            } else if !stderr.is_empty() {
                eprintln!("[tidepool-extract stderr]\n{stderr}");
            }
        },
    )?;

    // Store only what DESERIALIZED, so a malformed artifact set is never
    // memoized into a permanently-failing entry. Best-effort: an unwritable
    // memo costs a recompile, it never fails a compile.
    let artifacts = assemble(&meta_bytes, &raw, &mut on_stage)?;
    if let Some(key) = &eval_key {
        cache::cache_store(key, &raw[0].expr_bytes, &meta_bytes, &raw[0].asks_bytes);
    }
    if let Some(key) = &inv_key {
        store_memo(key, &name_refs, &meta_bytes, &raw);
    }
    Ok(artifacts)
}

/// Compile `source` against MULTIPLE named targets — the harness turn lane's
/// projection of [`compile_invocation`]. See its doc for the full contract
/// (target semantics, asks sidecar shape, memoization, timing hook).
pub fn compile_targets(
    source: &str,
    targets: &[&str],
    include: &[PathBuf],
    bin: Option<&ResolvedExtractBin>,
    on_stage: impl FnMut(&str, Duration, u64),
) -> Result<CompiledArtifacts, CompileError> {
    assert!(
        !targets.is_empty(),
        "compile_targets: at least one target is required"
    );
    let inv = CompileInvocation {
        source,
        targets,
        include,
        bin,
        fallback_module_name: "Expr",
        cache: CacheStrategy::Invocation,
        stable_val: None,
        session_inject: None,
    };
    compile_invocation(&inv, on_stage)
}

/// As [`compile_targets`], but additionally injects a [`StableValInject`]
/// (`--session-root <dir> --inject-val <module>`) — see that type's doc. The
/// self-iterating harness driver's fused outer render/loop compile is the
/// only caller.
pub fn compile_targets_with_stable_inject(
    source: &str,
    targets: &[&str],
    include: &[PathBuf],
    bin: Option<&ResolvedExtractBin>,
    stable_val: StableValInject<'_>,
    on_stage: impl FnMut(&str, Duration, u64),
) -> Result<CompiledArtifacts, CompileError> {
    assert!(
        !targets.is_empty(),
        "compile_targets_with_stable_inject: at least one target is required"
    );
    let inv = CompileInvocation {
        source,
        targets,
        include,
        bin,
        fallback_module_name: "Expr",
        cache: CacheStrategy::Invocation,
        stable_val: Some(stable_val),
        session_inject: None,
    };
    compile_invocation(&inv, on_stage)
}

/// As [`compile_targets`], but additionally injects a [`SessionInject`]
/// (`--session-root <dir>` plus one `--inject-val <module>` per live module)
/// and is NEVER memoized in [`tidepool_runtime::cache`] — see
/// [`SessionInject`]'s and [`CacheStrategy::Uncached`]'s docs for why.
pub fn compile_targets_with_session_inject(
    source: &str,
    targets: &[&str],
    include: &[PathBuf],
    bin: Option<&ResolvedExtractBin>,
    session_inject: SessionInject<'_>,
    on_stage: impl FnMut(&str, Duration, u64),
) -> Result<CompiledArtifacts, CompileError> {
    assert!(
        !targets.is_empty(),
        "compile_targets_with_session_inject: at least one target is required"
    );
    let inv = CompileInvocation {
        source,
        targets,
        include,
        bin,
        fallback_module_name: "Expr",
        cache: CacheStrategy::Uncached,
        stable_val: None,
        session_inject: Some(session_inject),
    };
    compile_invocation(&inv, on_stage)
}

// ---------------------------------------------------------------------------
// Shared spawn + read + deserialize (also used by `tidepool_runtime::compile_haskell`)
// ---------------------------------------------------------------------------

/// One target's raw (pre-deserialize) bytes.
pub(crate) struct RawTargetOutput {
    pub(crate) target: String,
    pub(crate) expr_bytes: Vec<u8>,
    asks_bytes: Vec<u8>,
}

/// Spawn `cmd` (already fully configured — input, output-dir, target(s),
/// includes) and pull `targets`' bytes off `temp_dir`, forwarding every
/// measured stage through `on_stage`. `log_stderr(text, success)` is called
/// once after the spawn, whatever the outcome, so each caller can apply its
/// own logging policy (`compile_haskell` always echoes non-empty stderr;
/// `compile_targets` only warns on failure).
///
/// A nonzero exit is read through the structured diagnostics contract
/// ([`diag::decode_extract_result`]) — the SAME reading `tidepool_runtime::compile_haskell`
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
    // Default-on per-compile summary (compile-attribution lane): unlike
    // `extract_timing` above, this is emitted by the extract UNCONDITIONALLY
    // (no `TIDEPOOL_TIMING` required) — see `Tidepool.Timing.emitCompileSummary`.
    // Logged at INFO so it lands in a plain harness log by default; absent on
    // a memo hit (this function isn't reached) or on a compile that threw
    // before reaching the summary line.
    if let Some(summary) = timing::CompileSummary::parse(&stderr) {
        timing::log_compile_summary(&summary);
    }
    // Full per-module breakdown (compile-attribution lane): DEBUG-gated,
    // present only when `TIDEPOOL_TIMING=1` reached the extract — see
    // `timing::log_module_timings`'s doc for why this stays a level below
    // the always-on summary above.
    let module_timings = timing::parse_module_timings(&stderr);
    if !module_timings.is_empty() {
        timing::log_module_timings(&module_timings);
    }
    log_stderr(&stderr, run.success());

    diag::decode_extract_result(run.success(), &run.output.stdout, &run.output.stderr)?;

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
        let asks = parse_asks(&r.asks_bytes)?;
        targets.insert(r.target.clone(), TargetArtifact { expr, asks });
    }
    on_stage(timing::STAGE_ASKS_PARSE, asks_start.elapsed(), 0);

    Ok(CompiledArtifacts {
        table,
        warnings,
        targets,
    })
}

/// Read the required typed-yield sidecar. The extractor writes `[]` for a
/// program with no sites; absence therefore means an incomplete or stale
/// compiler artifact, never "no effects".
fn read_asks_bytes(path: &Path) -> Result<Vec<u8>, CompileError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(CompileError::MissingOutput(path.to_path_buf()))
        }
        Err(e) => Err(CompileError::Io(e)),
    }
}

fn parse_asks(bytes: &[u8]) -> Result<YieldSites, CompileError> {
    let sites: Vec<YieldSite> =
        serde_json::from_slice(bytes).map_err(|e| CompileError::Asks(e.to_string()))?;
    YieldSites::from_sites(sites).map_err(|error| CompileError::Asks(error.to_string()))
}

/// Read the compiler's required typed-yield sidecar for session-turn callers.
/// This is the sole path-level reader; multi-target compilation uses the same
/// byte parser after its cache layer has captured the artifact set.
pub fn read_yield_sites(path: &Path) -> Result<Vec<YieldSite>, CompileError> {
    let bytes = read_asks_bytes(path)?;
    serde_json::from_slice(&bytes).map_err(|e| CompileError::Asks(e.to_string()))
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
            .map(|r| r.expr_bytes.len() + r.asks_bytes.len())
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
    // Every artifact is required. An extractor represents a target with no
    // typed suspension sites by writing an `[]` sidecar.
    let meta_bytes = it.next()??;
    let mut raw = Vec::with_capacity(targets.len());
    for target in targets {
        let expr_bytes = it.next()??;
        let asks_bytes = it.next()??;
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
        artifacts.push((asks_name, Some(r.asks_bytes.as_slice())));
    }
    cache::artifacts_store(key, &artifacts);
}

#[cfg(test)]
mod typed_site_tests {
    use super::*;

    fn site(id: u64, ty: &str) -> YieldSite {
        YieldSite {
            site: id,
            origin: "M.program".into(),
            ordinal: 0,
            ty: ty.into(),
            modules: Vec::new(),
            heads: Vec::new(),
            inputs: Vec::new(),
        }
    }

    #[test]
    fn missing_sidecar_is_not_an_empty_site_set() {
        let root = tempfile::tempdir().expect("temporary artifact root");
        let path = root.path().join("asks.json");
        assert!(matches!(
            read_yield_sites(&path),
            Err(CompileError::MissingOutput(missing)) if missing == path
        ));
    }

    #[test]
    fn empty_sidecar_is_the_only_empty_site_set() {
        let root = tempfile::tempdir().expect("temporary artifact root");
        let path = root.path().join("asks.json");
        std::fs::write(&path, b"[]").expect("write empty sidecar");
        assert_eq!(read_yield_sites(&path).expect("read empty sidecar"), vec![]);
    }

    #[test]
    fn conflicting_site_id_is_a_structured_error() {
        let collision = YieldSites::from_sites(vec![site(7, "Int"), site(7, "Bool")])
            .expect_err("conflicting metadata must fail");
        assert_eq!(collision.site, 7);
        assert_eq!(collision.first.ty, "Int");
        assert_eq!(collision.second.ty, "Bool");
    }
}
