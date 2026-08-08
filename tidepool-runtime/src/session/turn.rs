//! Wave 3b — session-eval turn compilation (the bind/reference extract seam).
//!
//! A `session_eval` turn is classified by GHC's parser (parse-only) into a BIND
//! (`x <- action` / `let x = e`), an EXPR (bare expression), or a DECL
//! (top-level declaration), then compiled through the session-aware extract
//! path with the live `Tidepool.Session.Val.G<g>` ifaces injected. On a BIND
//! turn the extract also writes the thin session iface (under `session_root`)
//! and returns the [`BoundBinder`]s this module decodes.
//!
//! [`run_turn`] performs exactly ONE `tidepool-extract --turn` spawn per call:
//! the extract classifies the turn itself (unless a caller-supplied verdict is
//! forwarded via `--turn-verdict`), picks its own wrapper template by
//! wire-name lookup, compiles, and returns the `TurnOut` CBOR sidecar this
//! module decodes into a [`TurnResult`]. Rust never classifies — every verdict
//! is GHC-sourced, whether it arrives from [`classify_block`]'s batch spawn or
//! from inside the turn spawn itself.
//!
//! These calls deliberately bypass the memo cache in [`crate::compile_haskell`]:
//! a session turn has on-disk side effects (the iface write) and depends on
//! mutable session state (the injected ifaces), so a cache hit would be wrong.

use std::path::Path;
use std::process::Command;

use ciborium::value::Value as CborValue;
use tempfile::TempDir;

use tidepool_repr::serial::{read_cbor, read_metadata, MetaWarnings};
use tidepool_repr::{CoreExpr, DataConTable};

use crate::{extract_module_name, CompileError};

use super::render::ExportItem;

/// Strict-force tier of a bound value (mirrors the extract's `BoundBinder.tier`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueTier {
    /// First-order data — `deep_force`d to NF then tenured.
    Tier0Data,
    /// A closure/PAP — tenured as-is (not forced).
    Tier1Closure,
}

/// One binder a BIND turn introduces — the extract's `BoundBinder` record.
#[derive(Clone, Debug)]
pub struct BoundBinder {
    /// The user-facing name (`"x"`).
    pub name: String,
    /// The `0xFE`-tagged stable id minted by `Translate.stableVarId`.
    pub var_id: u64,
    /// `Tidepool.Session.Val.G<g>` — the module whose thin iface was written.
    pub module: String,
    /// Tier of the bound value.
    pub tier: ValueTier,
    /// `ppr` of the bound value's type, for `:t`.
    pub type_display: String,
}

/// The three mutually-exclusive shapes a turn can take (GHC-sourced).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TurnKind {
    /// A top-level declaration (`f x = e`, `f :: T`, `x = 5`, `(a,b) = p`).
    Decl,
    /// A bind (`x <- e` / `let x = e`).
    Bind,
    /// A bare expression.
    Expr,
}

/// The wire-name string the extract's `--turn-verdict <kind>[:<names>]` and
/// the block-classify JSON `kind` field both use.
fn turn_kind_wire_name(kind: TurnKind) -> &'static str {
    match kind {
        TurnKind::Decl => "decl",
        TurnKind::Bind => "bind",
        TurnKind::Expr => "expr",
    }
}

/// `classifyTurn` rule 6: neither parse succeeds → `expr`, so the real error
/// surfaces at compile rather than here.
fn parse_turn_kind(kind: &str) -> TurnKind {
    match kind {
        "decl" => TurnKind::Decl,
        "bind" => TurnKind::Bind,
        _ => TurnKind::Expr,
    }
}

/// Decl-vs-bind-vs-expr classification of a turn (GHC-sourced, parse-only).
///
/// GHC's parser is the single authority (both declaration and statement
/// contexts are tried; see `Tidepool.Binders.classifyTurn`).
#[derive(Clone, Debug)]
pub struct TurnClassification {
    /// Which of the three mutually-exclusive turn shapes GHC parsed.
    pub kind: TurnKind,
    /// The bound/declared names (GHC-sourced). Empty for a bare expr.
    pub binders: Vec<String>,
}

/// The result of compiling one session-eval turn.
pub struct SessionTurnResult {
    /// JIT-able Core for the turn's `result` binding.
    pub expr: CoreExpr,
    /// This turn's DataCon metadata (the repl merges it into the session table).
    pub table: DataConTable,
    /// Compile warnings (e.g. `has_io`).
    pub warnings: MetaWarnings,
    /// The binder(s) this turn introduced — non-empty only on a BIND turn.
    pub binders: Vec<BoundBinder>,
    /// Typed-yield sites from the `asks.json` sidecar — `(site id, rendered
    /// answer type)`. The harness reads these to classify a `fork`/`dialogAsk`
    /// hole; the repl ignores them (its ask surface is untyped-site). Empty when
    /// the turn has no yield sites (or the sidecar is absent — an older extract).
    pub asks: Vec<(u32, String)>,
}

/// Arguments for the bind half of a turn (omit for an EXPR turn).
#[derive(Clone, Debug)]
pub struct SessionBind<'a> {
    /// The bound names (GHC-sourced). One name for a single-binder turn; N
    /// names for a flat-tuple multi-binder turn.
    pub names: &'a [String],
    /// The generation of the `Val.G<g>` module to mint (shared by all N names).
    pub gen: u64,
}

/// Which wrapper template a verdict selects. A refinement of [`TurnKind`]:
/// a `Bind` verdict maps to one of two distinct template shapes depending on
/// whether it actually binds a name (`binders.is_empty()`) — a discarding
/// bind (`_ <- e`) can't flow through [`SessionBind`] (the extract rejects an
/// empty `--bind-name` list), so it needs its own wrapper. [`TurnKind`] stays
/// a plain 3-value mirror of the extract's wire-contract `kind` string
/// ([`Decl`]/[`Bind`]/[`Expr`](TurnKind)); this is the separate, Rust-only
/// selection key template lookup is keyed on. `Decl` also selects a template
/// — the extract's decl path requires `--turn-template decl=<file>` and
/// errors without one — but a `Decl` verdict still never compiles through it;
/// the template is only the parse wrapper (`wrap_decls`'s pragma block).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TemplateSelector {
    /// A top-level declaration — selects the parse wrapper (never compiles).
    Decl,
    /// A bind that binds at least one name (`x <- e`, `let x = e`,
    /// `(a, b) <- e`).
    Bind,
    /// A bind whose pattern binds no name (`_ <- e`, `(_, _) <- e`): runs the
    /// statement for its effects and discards the result.
    BindDiscard,
    /// A bare expression.
    Expr,
}

impl TemplateSelector {
    /// Compute the selector a verdict maps to. Total over every [`TurnKind`]
    /// now that `Decl` also selects a template — kept `Option`-returning for
    /// a minimal signature; the `None` arm is unreachable.
    fn for_verdict(kind: TurnKind, binders: &[String]) -> Option<Self> {
        Some(match kind {
            TurnKind::Decl => TemplateSelector::Decl,
            TurnKind::Bind if binders.is_empty() => TemplateSelector::BindDiscard,
            TurnKind::Bind => TemplateSelector::Bind,
            TurnKind::Expr => TemplateSelector::Expr,
        })
    }

    /// The wire-name string the extract's `--turn-template <kind>=<file>`
    /// keys its template lookup on.
    fn wire_name(self) -> &'static str {
        match self {
            TemplateSelector::Decl => "decl",
            TemplateSelector::Bind => "bind",
            TemplateSelector::BindDiscard => "binddiscard",
            TemplateSelector::Expr => "expr",
        }
    }
}

/// A wrapper-module source for one [`TemplateSelector`], with two
/// SUBSTITUTION-POINT placeholders — `{{BINDERS}}` (the verdict's binder
/// names, comma-joined) — and two PLACEMENT modes for the turn text itself,
/// a property of where a template's splice point sits:
///
/// - `{{TURN}}` places the raw turn text VERBATIM.
/// - `{{TURN_STMT}}` places it as a `do`-block statement, applying exactly
///   the normalization the repl's `push_braced_stmt` performs today (a `let`
///   turn is rewritten to the layout-safe explicit-brace `let { … }` form, so
///   a `let` at column 1 can't break the block's layout). A template uses
///   exactly one of the two — this is two placement modes, not a template
///   language: no conditionals, loops, or further placeholders.
///
/// Byte-exactness is the contract: for a given verdict, the module the
/// extract compiles must be byte-identical to the module a caller wraps by
/// hand today (see [`render_template`], the byte-identity anchor).
///
/// Several [`TurnTemplate`]s may share a `kind`, forming an ordered variant
/// list — the extract's own retry: try each in order (per kind), first that
/// typechecks wins, `variant` (on the `Bind`/`Expr` result) reports which.
/// [`run_turn`] forwards every supplied template to the extract in order; it
/// does not pick a variant itself.
#[derive(Clone, Debug)]
pub struct TurnTemplate {
    pub kind: TemplateSelector,
    pub source: String,
}

/// The decl template source `run_turn`'s `Decl` verdict selects — the parse
/// wrapper the deleted `binders.rs`'s `wrap_decls` used to build per-call, now
/// authored once and spliced via `{{TURN}}`. The pragma block is
/// byte-identical to `wrap_decls`'s (moved verbatim, not retyped).
pub const DECL_TEMPLATE_SOURCE: &str = "{-# LANGUAGE GADTs, OverloadedStrings, TypeOperators, DataKinds, ScopedTypeVariables, BangPatterns, ViewPatterns, TupleSections, MultiWayIf, LambdaCase, RecordWildCards, NamedFieldPuns, DeriveFunctor, DeriveFoldable, DeriveTraversable, TypeApplications, QuasiQuotes #-}\nmodule SessionDecls where\n{{TURN}}\n";

/// One `run_turn` request: the raw turn text, the wrapper templates it may
/// need, the session context, the bind generation, and an optional
/// caller-supplied verdict.
///
/// When `verdict` is `Some`, [`run_turn`] forwards it to the extract via
/// `--turn-verdict`, skipping the extract's own re-parse — this is the
/// batch-classify case, where a caller already holds a GHC-sourced verdict
/// for the whole block (from [`classify_block`]). GHC-sourced either way.
pub struct TurnRequest<'a> {
    /// The raw turn text (`x <- e` / `let x = e` / a bare expression / a
    /// declaration), written to `turn.txt` and spliced by the extract into
    /// whichever template the verdict selects.
    pub turn_text: &'a str,
    /// Wrapper templates. Every template is written to its own file and
    /// passed as `--turn-template <kind>=<path>`, in the order supplied; the
    /// extract picks which one applies from its own verdict.
    pub templates: &'a [TurnTemplate],
    /// Extra `--include` dirs, forwarded to the extract on every call.
    pub include: &'a [&'a Path],
    /// Where the `Val` ifaces are written/read. Passed as `--session-root` on
    /// every call — the decl verdict doesn't use it, but the flag is
    /// unconditional in the one-spawn wire.
    pub session_root: &'a Path,
    /// Live `Tidepool.Session.Val.G<g'>` modules to inject, passed one per
    /// `--inject-val`.
    pub inject_modules: &'a [String],
    /// The generation of the `Val.G<g>` module a BIND turn mints, passed as
    /// `--bind-gen` on every call.
    pub gen: u64,
    /// A verdict the caller already holds from a batch classify
    /// ([`classify_block`]). Forwarded as `--turn-verdict`, skipping the
    /// extract's internal re-parse.
    pub verdict: Option<TurnClassification>,
    /// The Core binding a compiling template names as its target. `None`
    /// means the extract's scaffold-reserved default (`__result`), which is
    /// what a template authored for this path should use. Supply it only when
    /// a template's target binder is fixed by a builder shared with another
    /// path — `tidepool-harness`'s expression wrapper comes from
    /// `tidepool_mcp::template_haskell`, which the stateless eval server also
    /// uses and which names its target `result`. Forwarded as `--target`; the
    /// output file base is `result.cbor` either way.
    pub target: Option<&'a str>,
}

/// What a compiled (BIND or EXPR) turn yields. Grouped separately from
/// [`TurnResult`] so the `Decl` variant, which compiles nothing, carries none
/// of it.
#[derive(Debug)]
pub struct CompiledTurn {
    /// JIT-able Core for the turn's `result` binding.
    pub expr: CoreExpr,
    /// This turn's DataCon metadata.
    pub table: DataConTable,
    /// Compile warnings (e.g. `has_io`).
    pub warnings: MetaWarnings,
    /// Typed-yield sites, decoded from the `TurnOut` wire payload.
    pub asks: Vec<(u32, String)>,
}

/// The result of [`run_turn`] — one variant per verdict, each carrying only
/// its own kind's payload. A caller cannot read a field its verdict does not
/// have.
#[derive(Debug)]
pub enum TurnResult {
    /// A top-level declaration. Does not compile: `items` is the decl's
    /// export items, harvested by the extract's whole-module decl parse —
    /// NOT by the statement parse that serves the verdict, which cannot cover
    /// a multi-declaration batch.
    Decl {
        /// The declared names (GHC-sourced).
        binders: Vec<String>,
        items: Vec<ExportItem>,
    },
    /// A bind (`x <- e` / `let x = e`).
    Bind {
        /// The verdict's bound/declared names.
        binders: Vec<String>,
        /// The compiled bound-binder records.
        bound: Vec<BoundBinder>,
        /// Which template variant of the `Bind` kind compiled.
        variant: usize,
        compiled: CompiledTurn,
        /// The full wrapped module actually compiled — the byte-identity
        /// anchor.
        wrapped_source: String,
    },
    /// A bare expression.
    Expr {
        /// Which template variant of the `Expr` kind compiled.
        variant: usize,
        compiled: CompiledTurn,
        /// The full wrapped module actually compiled — the byte-identity
        /// anchor.
        wrapped_source: String,
    },
}

/// Place `turn_text` as a `do`-block statement — the `{{TURN_STMT}}`
/// placement mode. Mirrors `tidepool-repl`'s `push_braced_stmt` byte-for-byte:
/// a `let` turn (at column 1, since a raw turn has no leading indentation)
/// needs explicit decl braces there (a layout `let` swallows the following
/// `;`), so it is rewritten to `let { <rest> }`; anything else is placed
/// verbatim. Both branches guarantee a trailing newline so a template's own
/// following text always starts on a fresh line.
fn place_turn_stmt(turn_text: &str) -> String {
    let trimmed = turn_text.trim_start();
    let let_rest = trimmed
        .strip_prefix("let")
        .filter(|r| r.starts_with(|c: char| c.is_whitespace()));
    match let_rest {
        Some(rest) if !rest.trim_start().starts_with('{') => {
            let mut out = String::from("let {");
            out.push_str(rest);
            if !rest.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(" }\n");
            out
        }
        _ => {
            let mut out = turn_text.to_string();
            if !turn_text.ends_with('\n') {
                out.push('\n');
            }
            out
        }
    }
}

/// Splice `turn_text` and `binders` into a template `source`. `{{BINDERS}}`
/// is substituted first (it never depends on `turn_text`, so this is always
/// safe), then whichever ONE placement placeholder the template uses —
/// `{{TURN}}` (verbatim) or `{{TURN_STMT}}` (as a `do`-statement, see
/// [`place_turn_stmt`]) — is substituted last, so turn text containing
/// literal `{{BINDERS}}`/`{{TURN}}`/`{{TURN_STMT}}` text is never re-scanned
/// — the turn splice is byte-exact regardless of its content.
///
/// This is the byte-identity anchor `tidepool-repl`'s
/// `turn_template_byte_identity_tests` asserts against: the extract does its
/// own splice at runtime (this function isn't called from [`run_turn`]
/// anymore), but that test is what proves the two agree.
pub fn render_template(source: &str, turn_text: &str, binders: &[String]) -> String {
    let source = source.replace("{{BINDERS}}", &binders.join(", "));
    if source.contains("{{TURN_STMT}}") {
        source.replace("{{TURN_STMT}}", &place_turn_stmt(turn_text))
    } else {
        source.replace("{{TURN}}", turn_text)
    }
}

/// Find the first template in `templates` matching `selector` (declaration
/// order) — used by [`run_turn`]'s preflight check that a required template
/// was actually supplied before spawning the extract at all. The extract
/// receives every supplied template (an ordered `--turn-template` list per
/// kind) and does its own variant-retry selection; this lookup exists only to
/// fail fast, in Rust, on a caller wiring bug (a verdict with no matching
/// template) rather than let it surface as a confusing extract-side error.
fn select_template(
    templates: &[TurnTemplate],
    selector: TemplateSelector,
) -> Option<&TurnTemplate> {
    templates.iter().find(|t| t.kind == selector)
}

/// A missing template for a verdict is a caller wiring bug, not a GHC
/// rejection — reported the same way the turn lane reports every other
/// synthetic shape violation (`CompileError::ExtractFailed`, never a panic).
fn missing_template_error(selector: TemplateSelector) -> CompileError {
    CompileError::ExtractFailed(format!("run_turn: no template supplied for {selector:?}"))
}

fn extract_bin() -> String {
    std::env::var("TIDEPOOL_EXTRACT").unwrap_or_else(|_| "tidepool-extract".to_string())
}

fn map_notfound(e: std::io::Error) -> CompileError {
    CompileError::Io(crate::extract_spawn_error(e))
}

/// Scan `stderr` for `tidepool-timing phase=<name> ms=<int>` lines and re-emit
/// each as a `<prefix>.<phase>` stage via [`super::record_turn_stage`]. Mirrors
/// `tidepool-harness/src/timing.rs`'s `ExtractTiming::parse` +
/// `record_extract_phases`/`record_classify_phases`-shaped forwarding by hand
/// (see `record_turn_stage`'s doc for why this crate can't just import them).
///
/// `prefix` selects which `tidepool-extract` spawn these phases came from —
/// pass `"classify"` (matching `timing.rs`'s `CLASSIFY_STAGE_PREFIX`) for the
/// parse-only [`classify_block`] spawn, `"extract"` (matching
/// `EXTRACT_STAGE_PREFIX`) for a full-pipeline spawn like [`run_turn`]'s or
/// `compile_session_turn`'s. Two DIFFERENT subprocess spawns must never share
/// a prefix — a collector summing by stage name would silently merge their
/// costs into one row. Malformed lines are skipped (diagnostics only, never a
/// failure path); absent timing lines forward zero phases.
fn forward_extract_timing(stderr: &str, prefix: &str) {
    for line in stderr.lines() {
        let Some(rest) = line.trim().strip_prefix("tidepool-timing ") else {
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
        if let (Some(phase), Some(ms)) = (phase, ms) {
            let stage = format!("{prefix}.{phase}");
            super::record_turn_stage(&stage, std::time::Duration::from_millis(ms), 0);
        }
    }
}

/// The one entry point for a session-eval turn. Writes the turn text and
/// every supplied wrapper template to a [`TempDir`], then performs exactly
/// ONE `tidepool-extract --turn` spawn: `--turn-template <kind>=<path>` per
/// template (in order), `--turn-out`/`--output-dir` into the same temp dir,
/// `--include`/`--session-root`/`--inject-val`/`--bind-gen`, and
/// `--turn-verdict` when `req.verdict` is supplied (skipping the extract's
/// internal re-parse). Decodes the `TurnOut` CBOR sidecar into the matching
/// [`TurnResult`] variant; for `Bind`/`Expr` also reads `result.cbor` /
/// `meta.cbor` off the same output dir, exactly as [`compile_session_turn`]
/// does for its own compile.
///
/// When `req.verdict` is supplied, a missing template for the verdict's
/// selector is caught here, before any process is spawned.
pub fn run_turn(req: TurnRequest<'_>) -> Result<TurnResult, CompileError> {
    let verdict_arg = match &req.verdict {
        Some(TurnClassification { kind, binders }) => {
            let selector = TemplateSelector::for_verdict(*kind, binders)
                .expect("TemplateSelector::for_verdict is total over TurnKind");
            if select_template(req.templates, selector).is_none() {
                return Err(missing_template_error(selector));
            }
            let mut arg = turn_kind_wire_name(*kind).to_string();
            if !binders.is_empty() {
                arg.push(':');
                arg.push_str(&binders.join(","));
            }
            Some(arg)
        }
        None => None,
    };

    let temp = TempDir::new()?;
    let turn_path = temp.path().join("turn.txt");
    std::fs::write(&turn_path, req.turn_text)?;
    let turn_out_path = temp.path().join("turn.cbor");

    let mut cmd = Command::new(extract_bin());
    cmd.arg(&turn_path).arg("--turn");

    for (i, tmpl) in req.templates.iter().enumerate() {
        let path = temp.path().join(format!("template-{i}.hs"));
        std::fs::write(&path, &tmpl.source)?;
        cmd.arg("--turn-template")
            .arg(format!("{}={}", tmpl.kind.wire_name(), path.display()));
    }

    cmd.arg("--turn-out")
        .arg(&turn_out_path)
        .arg("--output-dir")
        .arg(temp.path());
    for p in req.include {
        cmd.arg("--include").arg(p);
    }
    cmd.arg("--session-root").arg(req.session_root);
    for m in req.inject_modules {
        cmd.arg("--inject-val").arg(m);
    }
    cmd.arg("--bind-gen").arg(req.gen.to_string());
    if let Some(target) = req.target {
        cmd.arg("--target").arg(target);
    }
    if let Some(arg) = verdict_arg {
        cmd.arg("--turn-verdict").arg(arg);
    }

    let spawn_start = std::time::Instant::now();
    let output = cmd.output().map_err(map_notfound)?;
    super::record_turn_stage("extract_spawn", spawn_start.elapsed(), 0);
    let stderr = String::from_utf8_lossy(&output.stderr);
    // A failed compile is still a real spawn — attribute its extract phases
    // the same as a successful one, before the early return below.
    forward_extract_timing(&stderr, "extract");
    if !output.status.success() {
        return Err(
            match crate::diag::parse_diag_report(&output.stdout, &output.stderr) {
                Ok(report) => CompileError::Diagnostics(report.diagnostics),
                Err(msg) => CompileError::MalformedDiagnostics(msg),
            },
        );
    }

    if !turn_out_path.exists() {
        return Err(CompileError::MissingOutput(turn_out_path));
    }
    let turn_out_bytes = std::fs::read(&turn_out_path)?;
    let turn_out = decode_turn_out(&turn_out_bytes)?;

    match turn_out {
        DecodedTurnOut::Decl { binders, items } => Ok(TurnResult::Decl { binders, items }),
        DecodedTurnOut::Bind {
            binders,
            variant,
            bound,
            asks,
            wrapped_source,
        } => {
            let compiled = read_compiled_turn(temp.path(), asks)?;
            Ok(TurnResult::Bind {
                binders,
                bound,
                variant,
                compiled,
                wrapped_source,
            })
        }
        DecodedTurnOut::Expr {
            variant,
            asks,
            wrapped_source,
        } => {
            let compiled = read_compiled_turn(temp.path(), asks)?;
            Ok(TurnResult::Expr {
                variant,
                compiled,
                wrapped_source,
            })
        }
    }
}

/// Read `result.cbor`/`meta.cbor` off `output_dir` and register warning var
/// names — the same post-compile bookkeeping [`compile_session_turn`]
/// performs, shared here because [`run_turn`]'s `Bind`/`Expr` arms need it
/// too. `asks` comes from the already-decoded wire variant, not a sidecar
/// read.
fn read_compiled_turn(
    output_dir: &Path,
    asks: Vec<(u32, String)>,
) -> Result<CompiledTurn, CompileError> {
    let expr_path = output_dir.join("result.cbor");
    let meta_path = output_dir.join("meta.cbor");
    if !expr_path.exists() {
        return Err(CompileError::MissingOutput(expr_path));
    }
    if !meta_path.exists() {
        return Err(CompileError::MissingOutput(meta_path));
    }
    let cbor_read_start = std::time::Instant::now();
    let expr_bytes = std::fs::read(&expr_path)?;
    let meta_bytes = std::fs::read(&meta_path)?;
    let cbor_read_bytes = (expr_bytes.len() + meta_bytes.len()) as u64;
    super::record_turn_stage("cbor_read", cbor_read_start.elapsed(), cbor_read_bytes);

    let deserialize_start = std::time::Instant::now();
    let expr = read_cbor(&expr_bytes)?;
    let (table, warnings) = read_metadata(&meta_bytes)?;
    super::record_turn_stage("cbor_deserialize", deserialize_start.elapsed(), 0);
    // Runtime unresolved-error naming (friction #12) — see lib.rs twin sites.
    tidepool_codegen::host_fns::register_var_names(&warnings.var_names);

    Ok(CompiledTurn {
        expr,
        table,
        warnings,
        asks,
    })
}

/// The decoded shape of the `TurnOut` CBOR sidecar, before `run_turn` reads
/// `result.cbor`/`meta.cbor` to build the final [`CompiledTurn`].
#[derive(Debug)]
enum DecodedTurnOut {
    Decl {
        binders: Vec<String>,
        items: Vec<ExportItem>,
    },
    Bind {
        binders: Vec<String>,
        variant: usize,
        bound: Vec<BoundBinder>,
        asks: Vec<(u32, String)>,
        wrapped_source: String,
    },
    Expr {
        variant: usize,
        asks: Vec<(u32, String)>,
        wrapped_source: String,
    },
}

fn cbor_shape_error(what: &str, expected: &str, got: &CborValue) -> CompileError {
    CompileError::ExtractFailed(format!(
        "TurnOut CBOR: expected {expected} for {what}, got {got:?}"
    ))
}

fn cbor_expect_array<'a>(v: &'a CborValue, what: &str) -> Result<&'a [CborValue], CompileError> {
    match v {
        CborValue::Array(a) => Ok(a),
        other => Err(cbor_shape_error(what, "array", other)),
    }
}

fn cbor_expect_array_len<'a>(
    v: &'a CborValue,
    n: usize,
    what: &str,
) -> Result<&'a [CborValue], CompileError> {
    let a = cbor_expect_array(v, what)?;
    if a.len() != n {
        return Err(CompileError::ExtractFailed(format!(
            "TurnOut CBOR: expected {what} array of length {n}, got {}",
            a.len()
        )));
    }
    Ok(a)
}

fn cbor_expect_text<'a>(v: &'a CborValue, what: &str) -> Result<&'a str, CompileError> {
    match v {
        CborValue::Text(t) => Ok(t.as_str()),
        other => Err(cbor_shape_error(what, "text", other)),
    }
}

fn cbor_as_u64(v: &CborValue, what: &str) -> Result<u64, CompileError> {
    match v {
        CborValue::Integer(i) => u64::try_from(*i).map_err(|_| cbor_shape_error(what, "u64", v)),
        other => Err(cbor_shape_error(what, "integer", other)),
    }
}

fn cbor_as_usize(v: &CborValue, what: &str) -> Result<usize, CompileError> {
    let u = cbor_as_u64(v, what)?;
    usize::try_from(u)
        .map_err(|_| CompileError::ExtractFailed(format!("TurnOut CBOR: {what} too large")))
}

fn decode_string_array(v: &CborValue, what: &str) -> Result<Vec<String>, CompileError> {
    cbor_expect_array(v, what)?
        .iter()
        .map(|s| cbor_expect_text(s, what).map(str::to_string))
        .collect()
}

fn decode_export_item(v: &CborValue) -> Result<ExportItem, CompileError> {
    let arr = cbor_expect_array(v, "ExportItem")?;
    let tag = arr
        .first()
        .ok_or_else(|| CompileError::ExtractFailed("TurnOut CBOR: empty ExportItem array".into()))
        .and_then(|t| cbor_expect_text(t, "ExportItem tag"))?;
    match (tag, arr.len()) {
        ("EValue", 2) => Ok(ExportItem::Value {
            name: cbor_expect_text(&arr[1], "EValue name")?.to_string(),
        }),
        ("EType", 3) => Ok(ExportItem::Type {
            name: cbor_expect_text(&arr[1], "EType name")?.to_string(),
            cons: decode_string_array(&arr[2], "EType cons")?,
        }),
        ("EClass", 3) => Ok(ExportItem::Class {
            name: cbor_expect_text(&arr[1], "EClass name")?.to_string(),
            methods: decode_string_array(&arr[2], "EClass methods")?,
        }),
        (tag, len) => Err(CompileError::ExtractFailed(format!(
            "TurnOut CBOR: unknown ExportItem tag {tag:?} with arity {len}"
        ))),
    }
}

fn decode_export_items(v: &CborValue) -> Result<Vec<ExportItem>, CompileError> {
    cbor_expect_array(v, "declItems")?
        .iter()
        .map(decode_export_item)
        .collect()
}

fn decode_bound_binder(v: &CborValue) -> Result<BoundBinder, CompileError> {
    let arr = cbor_expect_array_len(v, 5, "BoundBinder")?;
    let name = cbor_expect_text(&arr[0], "BoundBinder name")?.to_string();
    let var_id = cbor_as_u64(&arr[1], "BoundBinder varId")?;
    let module = cbor_expect_text(&arr[2], "BoundBinder module")?.to_string();
    let tier = match cbor_expect_text(&arr[3], "BoundBinder tier")? {
        "Tier1Closure" => ValueTier::Tier1Closure,
        "Tier0Data" => ValueTier::Tier0Data,
        other => {
            return Err(CompileError::ExtractFailed(format!(
                "TurnOut CBOR: unknown BoundBinder tier {other:?}"
            )))
        }
    };
    let type_display = cbor_expect_text(&arr[4], "BoundBinder typeDisplay")?.to_string();
    Ok(BoundBinder {
        name,
        var_id,
        module,
        tier,
        type_display,
    })
}

fn decode_bound_binders(v: &CborValue) -> Result<Vec<BoundBinder>, CompileError> {
    cbor_expect_array(v, "boundBinders")?
        .iter()
        .map(decode_bound_binder)
        .collect()
}

fn decode_ask(v: &CborValue) -> Result<(u32, String), CompileError> {
    let arr = cbor_expect_array_len(v, 2, "Ask")?;
    let site = cbor_as_u64(&arr[0], "Ask site")?;
    let site = u32::try_from(site).map_err(|_| {
        CompileError::ExtractFailed("TurnOut CBOR: Ask site too large for u32".into())
    })?;
    let answer_type = cbor_expect_text(&arr[1], "Ask answer type")?.to_string();
    Ok((site, answer_type))
}

fn decode_asks(v: &CborValue) -> Result<Vec<(u32, String)>, CompileError> {
    cbor_expect_array(v, "asks")?
        .iter()
        .map(decode_ask)
        .collect()
}

/// Decode the bare (no `TPLR` header — that belongs to the tree wire format
/// only) `TurnOut` CBOR value: a tagged 2-element list `[tag, payload]`. A
/// shape mismatch (wrong tag, wrong arity, wrong type) is a clean
/// [`CompileError::ExtractFailed`] naming what was expected, never a panic.
fn decode_turn_out(bytes: &[u8]) -> Result<DecodedTurnOut, CompileError> {
    let value: CborValue = ciborium::de::from_reader(bytes)
        .map_err(|e| CompileError::ExtractFailed(format!("TurnOut CBOR: malformed: {e}")))?;
    let root = cbor_expect_array_len(&value, 2, "TurnOut")?;
    let tag = cbor_expect_text(&root[0], "TurnOut tag")?;
    match tag {
        "Decl" => {
            let payload = cbor_expect_array_len(&root[1], 2, "Decl payload")?;
            let binders = decode_string_array(&payload[0], "Decl binders")?;
            let items = decode_export_items(&payload[1])?;
            Ok(DecodedTurnOut::Decl { binders, items })
        }
        "Bind" => {
            let payload = cbor_expect_array_len(&root[1], 5, "Bind payload")?;
            let binders = decode_string_array(&payload[0], "Bind binders")?;
            let variant = cbor_as_usize(&payload[1], "Bind variant")?;
            let bound = decode_bound_binders(&payload[2])?;
            let asks = decode_asks(&payload[3])?;
            let wrapped_source = cbor_expect_text(&payload[4], "Bind wrappedSource")?.to_string();
            Ok(DecodedTurnOut::Bind {
                binders,
                variant,
                bound,
                asks,
                wrapped_source,
            })
        }
        "Expr" => {
            let payload = cbor_expect_array_len(&root[1], 3, "Expr payload")?;
            let variant = cbor_as_usize(&payload[0], "Expr variant")?;
            let asks = decode_asks(&payload[1])?;
            let wrapped_source = cbor_expect_text(&payload[2], "Expr wrappedSource")?.to_string();
            Ok(DecodedTurnOut::Expr {
                variant,
                asks,
                wrapped_source,
            })
        }
        other => Err(CompileError::ExtractFailed(format!(
            "TurnOut CBOR: unknown tag {other:?}"
        ))),
    }
}

/// One parse-only extract spawn classifying N items in order. A repl block
/// runner needs verdicts for a whole block before compiling any item (it
/// segments consecutive decl-shaped items into one generation, and a
/// statement item may reference decls earlier in the same block), so it
/// classifies the block in one spawn rather than one spawn per item. One GHC
/// session boots for the whole batch.
pub fn classify_block(items: &[&str]) -> Result<Vec<TurnClassification>, CompileError> {
    if items.is_empty() {
        // With no positional files the extract falls through to its usage
        // branch, exits 0, and writes no `--classify-out` file — spawning
        // would surface as a confusing missing-file error for what is
        // obviously a no-op.
        return Ok(Vec::new());
    }
    let temp = TempDir::new()?;
    let mut paths = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let path = temp.path().join(format!("item-{i}.hs"));
        std::fs::write(&path, item)?;
        paths.push(path);
    }
    let out_path = temp.path().join("classify.json");

    let mut cmd = Command::new(extract_bin());
    for path in &paths {
        cmd.arg(path);
    }
    cmd.arg("--classify").arg("--classify-out").arg(&out_path);

    let output = cmd.output().map_err(map_notfound)?;
    // A failed classification still cost a real subprocess spawn — attribute
    // its extract phases the same as a successful one, before the early
    // return below.
    forward_extract_timing(&String::from_utf8_lossy(&output.stderr), "classify");
    if !output.status.success() {
        // A parsed report is a real GHC rejection of one of the items. An
        // UNPARSEABLE report is a stale/skewed extractor — `MalformedDiagnostics`
        // (→ VersionSkew), same as every other extract call site.
        let report = match crate::diag::parse_diag_report(&output.stdout, &output.stderr) {
            Ok(report) => report,
            Err(msg) => return Err(CompileError::MalformedDiagnostics(msg)),
        };
        let text = report
            .diagnostics
            .iter()
            .map(|d| d.message.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        return Err(CompileError::ExtractFailed(text));
    }

    let json = std::fs::read_to_string(&out_path).map_err(CompileError::Io)?;
    parse_classify_json(&json, items.len())
}

/// Parse `{"verdicts":[{kind,binders}, ...]}`. A verdict count that does not
/// match the item count is a clean, loud [`CompileError::ExtractFailed`] — a
/// silent length mismatch would misalign every downstream item against the
/// wrong verdict.
fn parse_classify_json(
    json: &str,
    expected: usize,
) -> Result<Vec<TurnClassification>, CompileError> {
    let v: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| CompileError::ExtractFailed(format!("invalid classify-block JSON: {e}")))?;
    let verdicts = v
        .get("verdicts")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            CompileError::ExtractFailed("classify-block JSON missing `verdicts`".into())
        })?;
    if verdicts.len() != expected {
        return Err(CompileError::ExtractFailed(format!(
            "classify-block: expected {expected} verdict(s), got {}",
            verdicts.len()
        )));
    }
    verdicts.iter().map(parse_one_verdict).collect()
}

fn parse_one_verdict(v: &serde_json::Value) -> Result<TurnClassification, CompileError> {
    let kind = v
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("expr");
    let binders = v
        .get("binders")
        .and_then(serde_json::Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Ok(TurnClassification {
        kind: parse_turn_kind(kind),
        binders,
    })
}

/// Compile one session-eval turn through the session-aware extract path.
///
/// `wrapped_source` is the full wrapped module (target binder `result`).
/// `inject_modules` are the live `Tidepool.Session.Val.G<g'>` module names to
/// inject so the turn can reference earlier bindings. `session_root` is where
/// the `Val` ifaces are written/read. `bind` carries the new binder's name+gen
/// on a BIND turn (and triggers the thin-iface write + sidecar emission).
pub fn compile_session_turn(
    wrapped_source: &str,
    include: &[&Path],
    session_root: &Path,
    inject_modules: &[String],
    bind: Option<SessionBind<'_>>,
) -> Result<SessionTurnResult, CompileError> {
    let temp = TempDir::new()?;
    let filename = extract_module_name(wrapped_source)
        .map_or_else(|| "Input.hs".to_string(), |m| format!("{m}.hs"));
    let input = temp.path().join(&filename);
    std::fs::write(&input, wrapped_source)?;
    let bb_path = temp.path().join("bound_binders.json");

    let mut cmd = Command::new(extract_bin());
    cmd.arg(&input)
        .arg("--output-dir")
        .arg(temp.path())
        // Scaffold-reserved binding name (never a plain user-choosable
        // identifier like "result") — Main.hs's session path always compiles
        // this exact target but still writes the output as result.cbor
        // below, so this rename needs no change to the read-back path.
        .arg("--target")
        .arg("__result")
        .arg("--session-root")
        .arg(session_root);
    for m in inject_modules {
        cmd.arg("--inject-val").arg(m);
    }
    for p in include {
        cmd.arg("--include").arg(p);
    }
    let is_bind = bind.is_some();
    if let Some(ref b) = bind {
        cmd.arg("--session-bind")
            .arg("--bind-gen")
            .arg(b.gen.to_string())
            .arg("--emit-bound-binders")
            .arg(&bb_path);
        for name in b.names {
            cmd.arg("--bind-name").arg(name);
        }
    }

    let spawn_start = std::time::Instant::now();
    let output = cmd.output().map_err(map_notfound)?;
    super::record_turn_stage("extract_spawn", spawn_start.elapsed(), 0);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.is_empty() {
        eprintln!("[tidepool-extract stderr]\n{stderr}");
    }
    // A failed compile is still a real answerer round — attribute its extract
    // phases the same as a successful one, before the early return below.
    // "extract", not "classify": this is a full-pipeline spawn, the same lane
    // `compile.rs::compile_turn` instruments.
    forward_extract_timing(&stderr, "extract");
    if !output.status.success() {
        return Err(
            match crate::diag::parse_diag_report(&output.stdout, &output.stderr) {
                Ok(report) => CompileError::Diagnostics(report.diagnostics),
                Err(msg) => CompileError::MalformedDiagnostics(msg),
            },
        );
    }

    let expr_path = temp.path().join("result.cbor");
    let meta_path = temp.path().join("meta.cbor");
    if !expr_path.exists() {
        return Err(CompileError::MissingOutput(expr_path));
    }
    if !meta_path.exists() {
        return Err(CompileError::MissingOutput(meta_path));
    }
    let cbor_read_start = std::time::Instant::now();
    let expr_bytes = std::fs::read(&expr_path)?;
    let meta_bytes = std::fs::read(&meta_path)?;
    let cbor_read_bytes = (expr_bytes.len() + meta_bytes.len()) as u64;
    super::record_turn_stage("cbor_read", cbor_read_start.elapsed(), cbor_read_bytes);

    let deserialize_start = std::time::Instant::now();
    let expr = read_cbor(&expr_bytes)?;
    let (table, warnings) = read_metadata(&meta_bytes)?;
    super::record_turn_stage("cbor_deserialize", deserialize_start.elapsed(), 0);
    // Runtime unresolved-error naming (friction #12) — see lib.rs twin sites.
    tidepool_codegen::host_fns::register_var_names(&warnings.var_names);

    let binders = if is_bind {
        let json = std::fs::read_to_string(&bb_path).map_err(|e| {
            CompileError::ExtractFailed(format!("bind turn emitted no bound-binder sidecar: {e}"))
        })?;
        parse_bound_binders(&json)?
    } else {
        Vec::new()
    };

    let asks_start = std::time::Instant::now();
    let asks = read_asks_sidecar(&temp.path().join("asks.json"))?;
    super::record_turn_stage("asks_parse", asks_start.elapsed(), 0);

    Ok(SessionTurnResult {
        expr,
        table,
        warnings,
        binders,
        asks,
    })
}

/// Read the `asks.json` typed-yield sidecar the extract writes into the
/// output-dir — `[{ "site": u32, "type": String }, …]`. A missing file yields
/// an empty list (a turn with no yield sites, or an older extract); a present
/// but malformed file is a hard error (a real sidecar-shape regression).
fn read_asks_sidecar(path: &Path) -> Result<Vec<(u32, String)>, CompileError> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(CompileError::Io(e)),
    };
    #[derive(serde::Deserialize)]
    struct Site {
        site: u32,
        #[serde(rename = "type")]
        ty: String,
    }
    let sites: Vec<Site> = serde_json::from_slice(&bytes)
        .map_err(|e| CompileError::ExtractFailed(format!("invalid asks.json sidecar: {e}")))?;
    Ok(sites.into_iter().map(|s| (s.site, s.ty)).collect())
}

fn parse_bound_binders(json: &str) -> Result<Vec<BoundBinder>, CompileError> {
    let v: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| CompileError::ExtractFailed(format!("invalid bound-binder JSON: {e}")))?;
    let arr = v
        .get("binders")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| CompileError::ExtractFailed("bound-binder JSON missing `binders`".into()))?;
    arr.iter().map(parse_one_binder).collect()
}

fn parse_one_binder(v: &serde_json::Value) -> Result<BoundBinder, CompileError> {
    let s = |k: &str| v.get(k).and_then(serde_json::Value::as_str);
    let name = s("name")
        .ok_or_else(|| CompileError::ExtractFailed("binder missing `name`".into()))?
        .to_string();
    // varId is a DECIMAL STRING (JSON f64 would truncate a 64-bit id).
    let var_id = s("varId")
        .ok_or_else(|| CompileError::ExtractFailed("binder missing `varId`".into()))?
        .parse::<u64>()
        .map_err(|e| CompileError::ExtractFailed(format!("binder varId not a u64: {e}")))?;
    let module = s("module")
        .ok_or_else(|| CompileError::ExtractFailed("binder missing `module`".into()))?
        .to_string();
    let tier = match s("tier") {
        Some("Tier1Closure") => ValueTier::Tier1Closure,
        Some("Tier0Data") | None => ValueTier::Tier0Data,
        Some(other) => {
            return Err(CompileError::ExtractFailed(format!(
                "unknown binder tier {other:?}"
            )))
        }
    };
    let type_display = s("typeDisplay").unwrap_or("").to_string();
    Ok(BoundBinder {
        name,
        var_id,
        module,
        tier,
        type_display,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An extractor whose non-zero-exit stdout does not parse as the
    /// diagnostics report is a stale/skewed build: `MalformedDiagnostics`
    /// (→ VersionSkew), never `ExtractFailed` (→ UserHaskell) — same contract
    /// as `compile_session_turn` and `lib.rs::compile_haskell`. Env mutation is
    /// safe: nextest runs each test in its own process.
    #[test]
    fn classify_block_unparseable_report_is_malformed_diagnostics() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let fake = dir.path().join("fake-extract");
        std::fs::write(&fake, "#!/bin/sh\necho not-a-diag-report\nexit 1\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::env::set_var("TIDEPOOL_EXTRACT", &fake);
        let err = classify_block(&["x <- pure 1"]).unwrap_err();
        assert!(
            matches!(err, CompileError::MalformedDiagnostics(_)),
            "expected MalformedDiagnostics, got {err:?}"
        );
    }

    /// An empty item list must short-circuit in Rust and never spawn the
    /// extractor: with no positional files the extract falls through to its
    /// usage branch and writes no `--classify-out` file, so spawning would
    /// surface as a confusing missing-file error for what is obviously a
    /// no-op.
    #[test]
    fn classify_block_empty_items_short_circuits_without_spawning() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let log_path = dir.path().join("calls.log");
        let fake = dir.path().join("fake-extract");
        std::fs::write(
            &fake,
            format!("#!/bin/sh\necho \"$@\" >> {}\nexit 0\n", log_path.display()),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::env::set_var("TIDEPOOL_EXTRACT", &fake);

        let result = classify_block(&[]).unwrap();
        assert!(result.is_empty());
        assert!(
            !log_path.exists(),
            "classify_block(&[]) spawned the extractor"
        );
    }

    #[test]
    fn classify_block_verdict_count_mismatch_is_clean_error() {
        let err = parse_classify_json(r#"{"verdicts":[{"kind":"bind","binders":["x"]}]}"#, 2)
            .unwrap_err();
        assert!(
            matches!(err, CompileError::ExtractFailed(_)),
            "expected ExtractFailed, got {err:?}"
        );
    }

    #[test]
    fn parses_bind_classification() {
        let cs =
            parse_classify_json(r#"{"verdicts":[{"kind":"bind","binders":["x"]}]}"#, 1).unwrap();
        assert_eq!(cs[0].kind, TurnKind::Bind);
        assert_eq!(cs[0].binders, vec!["x".to_string()]);
    }

    #[test]
    fn parses_expr_classification() {
        let cs = parse_classify_json(r#"{"verdicts":[{"kind":"expr","binders":[]}]}"#, 1).unwrap();
        assert_eq!(cs[0].kind, TurnKind::Expr);
        assert!(cs[0].binders.is_empty());
    }

    #[test]
    fn parses_decl_classification() {
        let cs =
            parse_classify_json(r#"{"verdicts":[{"kind":"decl","binders":["sq"]}]}"#, 1).unwrap();
        assert_eq!(cs[0].kind, TurnKind::Decl);
        assert_eq!(cs[0].binders, vec!["sq".to_string()]);
    }

    #[test]
    fn parses_bound_binder_with_string_varid() {
        let raw = (0xFEu64 << 56) | 0x123456;
        let json = format!(
            r#"{{"binders":[{{"name":"x","varId":"{raw}","module":"Tidepool.Session.Val.G3","tier":"Tier0Data","typeDisplay":"Int"}}]}}"#
        );
        let bs = parse_bound_binders(&json).unwrap();
        assert_eq!(bs.len(), 1);
        assert_eq!(bs[0].name, "x");
        assert_eq!(bs[0].var_id, raw);
        assert_eq!(bs[0].tier, ValueTier::Tier0Data);
        assert_eq!(bs[0].module, "Tidepool.Session.Val.G3");
    }

    #[test]
    fn parses_tier1_closure() {
        let json = r#"{"binders":[{"name":"f","varId":"42","module":"Tidepool.Session.Val.G1","tier":"Tier1Closure","typeDisplay":"Int -> Int"}]}"#;
        let bs = parse_bound_binders(json).unwrap();
        assert_eq!(bs[0].tier, ValueTier::Tier1Closure);
    }

    #[test]
    fn render_template_byte_exact_turn_and_multi_binder_join() {
        let source = "module M where\nresult = do { {{TURN}}\n ; pure ({{BINDERS}}) }\n";
        let turn = "x <- pure {1, 2}\nlet y = {\"k\":1}";
        let out = render_template(source, turn, &["a".to_string(), "b".to_string()]);
        assert!(
            out.contains(turn),
            "turn text with braces/newlines was not spliced byte-exact:\n{out}"
        );
        assert!(
            out.contains("pure (a, b)"),
            "binders not comma-joined:\n{out}"
        );
        assert!(!out.contains("{{TURN}}"));
        assert!(!out.contains("{{BINDERS}}"));
    }

    #[test]
    fn render_template_single_binder_has_no_trailing_comma() {
        let out = render_template("{{BINDERS}}", "ignored", &["x".to_string()]);
        assert_eq!(out, "x");
    }

    /// `{{BINDERS}}` is substituted first and `{{TURN}}` last, so a turn text
    /// that happens to contain literal placeholder syntax is never re-scanned
    /// — the turn splice stays byte-exact regardless of its content.
    #[test]
    fn render_template_turn_text_placeholder_syntax_is_not_resubstituted() {
        let out = render_template(
            "A={{BINDERS}} T={{TURN}}",
            "contains {{BINDERS}} literally",
            &["z".to_string()],
        );
        assert_eq!(out, "A=z T=contains {{BINDERS}} literally");
    }

    #[test]
    fn select_template_picks_first_matching_kind_variant_zero() {
        let templates = vec![
            TurnTemplate {
                kind: TemplateSelector::Bind,
                source: "bind-a".to_string(),
            },
            TurnTemplate {
                kind: TemplateSelector::Expr,
                source: "expr-a".to_string(),
            },
            TurnTemplate {
                kind: TemplateSelector::Bind,
                source: "bind-b".to_string(),
            },
        ];
        assert_eq!(
            select_template(&templates, TemplateSelector::Bind)
                .unwrap()
                .source,
            "bind-a"
        );
        assert_eq!(
            select_template(&templates, TemplateSelector::Expr)
                .unwrap()
                .source,
            "expr-a"
        );
        assert!(select_template(&templates, TemplateSelector::BindDiscard).is_none());
    }

    #[test]
    fn template_selector_for_verdict_four_shapes() {
        assert_eq!(
            TemplateSelector::for_verdict(TurnKind::Decl, &[]),
            Some(TemplateSelector::Decl)
        );
        assert_eq!(
            TemplateSelector::for_verdict(TurnKind::Bind, &["x".to_string()]),
            Some(TemplateSelector::Bind)
        );
        assert_eq!(
            TemplateSelector::for_verdict(TurnKind::Bind, &["a".to_string(), "b".to_string()]),
            Some(TemplateSelector::Bind),
            "a multi-binder bind is still the named-bind selector"
        );
        assert_eq!(
            TemplateSelector::for_verdict(TurnKind::Bind, &[]),
            Some(TemplateSelector::BindDiscard)
        );
        assert_eq!(
            TemplateSelector::for_verdict(TurnKind::Expr, &[]),
            Some(TemplateSelector::Expr)
        );
    }

    #[test]
    fn render_template_turn_stmt_places_verbatim_when_not_a_let() {
        let source = "M {{TURN_STMT}} ; pure ({{BINDERS}})\n";
        let out = render_template(source, "x <- pure 1", &["x".to_string()]);
        assert_eq!(out, "M x <- pure 1\n ; pure (x)\n");
    }

    /// The case Part 2 of the spec calls out: a bind turn whose text begins
    /// with `let ` must be rewritten to the layout-safe explicit-brace form,
    /// matching `tidepool-repl`'s `push_braced_stmt` exactly — a verbatim
    /// `{{TURN}}` splice cannot reproduce this (a `let` at column 1 would
    /// break the enclosing `do` block's layout).
    #[test]
    fn render_template_turn_stmt_rewrites_column_1_let() {
        let source = "{{TURN_STMT}}pure ({{BINDERS}})\n";
        let out = render_template(source, "let y = 2", &["y".to_string()]);
        assert_eq!(out, "let { y = 2\n }\npure (y)\n");
    }

    #[test]
    fn render_template_turn_stmt_and_turn_text_containing_placeholder_syntax_not_resubstituted() {
        let out = render_template("S={{TURN_STMT}}", "contains {{TURN_STMT}} literally", &[]);
        assert_eq!(out, "S=contains {{TURN_STMT}} literally\n");
    }

    /// A verdict for a kind with no matching template is a clean error, not a
    /// panic — and it must not spawn the extractor at all (the lookup fails
    /// before any process is invoked).
    #[test]
    fn run_turn_missing_template_is_clean_error_not_panic() {
        std::env::set_var("TIDEPOOL_EXTRACT", "/nonexistent/tidepool-extract-test");
        let req = TurnRequest {
            turn_text: "1 + 1",
            templates: &[],
            include: &[],
            session_root: Path::new("/nonexistent-session-root"),
            inject_modules: &[],
            gen: 0,
            verdict: Some(TurnClassification {
                kind: TurnKind::Expr,
                binders: Vec::new(),
            }),
            target: None,
        };
        let err = run_turn(req).unwrap_err();
        assert!(
            matches!(err, CompileError::ExtractFailed(_)),
            "expected a clean ExtractFailed, got {err:?}"
        );
    }

    /// `run_turn` must spawn the extractor EXACTLY ONCE, carrying `--turn`,
    /// never a deleted classify/binder flag. The fake extractor logs its argv
    /// and exits non-zero (the compile is allowed to fail; only the spawn
    /// count and arguments matter here). Env mutation is safe: nextest runs
    /// each test in its own process.
    #[test]
    fn run_turn_spawns_extract_exactly_once_with_turn_flag() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let log_path = dir.path().join("calls.log");
        let fake = dir.path().join("fake-extract");
        std::fs::write(
            &fake,
            format!("#!/bin/sh\necho \"$@\" >> {}\nexit 1\n", log_path.display()),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::env::set_var("TIDEPOOL_EXTRACT", &fake);

        let session_root = dir.path().join("session");
        std::fs::create_dir_all(&session_root).unwrap();
        let templates = vec![TurnTemplate {
            kind: TemplateSelector::Expr,
            source: "module M where\nresult = {{TURN}}\n".to_string(),
        }];
        let req = TurnRequest {
            turn_text: "1 + 1",
            templates: &templates,
            include: &[],
            session_root: &session_root,
            inject_modules: &[],
            gen: 0,
            verdict: Some(TurnClassification {
                kind: TurnKind::Expr,
                binders: Vec::new(),
            }),
            target: None,
        };
        let _ = run_turn(req);

        let calls = std::fs::read_to_string(&log_path).unwrap_or_default();
        assert_eq!(
            calls.lines().count(),
            1,
            "expected exactly one extract spawn, got:\n{calls}"
        );
        let call = calls.lines().next().unwrap_or_default();
        assert!(call.contains("--turn"), "spawn missing --turn:\n{call}");
        assert!(
            !call.contains("--emit-stmt-binders") && !call.contains("--emit-binders"),
            "spawn carried a deleted classify/binder flag:\n{call}"
        );
    }

    // ---- TurnOut CBOR decoding ----

    fn build_cbor(v: &CborValue) -> Vec<u8> {
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(v, &mut bytes).unwrap();
        bytes
    }

    #[test]
    fn decode_turn_out_decl_variant() {
        let v = CborValue::Array(vec![
            CborValue::Text("Decl".into()),
            CborValue::Array(vec![
                CborValue::Array(vec![CborValue::Text("sq".into())]),
                CborValue::Array(vec![CborValue::Array(vec![
                    CborValue::Text("EValue".into()),
                    CborValue::Text("sq".into()),
                ])]),
            ]),
        ]);
        match decode_turn_out(&build_cbor(&v)).unwrap() {
            DecodedTurnOut::Decl { binders, items } => {
                assert_eq!(binders, vec!["sq".to_string()]);
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].head_name(), "sq");
            }
            other => panic!("expected Decl, got {other:?}"),
        }
    }

    #[test]
    fn decode_turn_out_bind_variant() {
        let v = CborValue::Array(vec![
            CborValue::Text("Bind".into()),
            CborValue::Array(vec![
                CborValue::Array(vec![CborValue::Text("x".into())]),
                CborValue::Integer(0.into()),
                CborValue::Array(vec![CborValue::Array(vec![
                    CborValue::Text("x".into()),
                    CborValue::Integer(42.into()),
                    CborValue::Text("Tidepool.Session.Val.G3".into()),
                    CborValue::Text("Tier0Data".into()),
                    CborValue::Text("Int".into()),
                ])]),
                CborValue::Array(vec![CborValue::Array(vec![
                    CborValue::Integer(7.into()),
                    CborValue::Text("Text".into()),
                ])]),
                CborValue::Text("module M where\nresult = x <- pure 1\n".into()),
            ]),
        ]);
        match decode_turn_out(&build_cbor(&v)).unwrap() {
            DecodedTurnOut::Bind {
                binders,
                variant,
                bound,
                asks,
                wrapped_source,
            } => {
                assert_eq!(binders, vec!["x".to_string()]);
                assert_eq!(variant, 0);
                assert_eq!(bound.len(), 1);
                assert_eq!(bound[0].name, "x");
                assert_eq!(bound[0].var_id, 42);
                assert_eq!(bound[0].tier, ValueTier::Tier0Data);
                assert_eq!(asks, vec![(7, "Text".to_string())]);
                assert!(wrapped_source.contains("result ="));
            }
            other => panic!("expected Bind, got {other:?}"),
        }
    }

    #[test]
    fn decode_turn_out_expr_variant() {
        let v = CborValue::Array(vec![
            CborValue::Text("Expr".into()),
            CborValue::Array(vec![
                CborValue::Integer(1.into()),
                CborValue::Array(vec![]),
                CborValue::Text("module M where\nresult = 1 + 1\n".into()),
            ]),
        ]);
        match decode_turn_out(&build_cbor(&v)).unwrap() {
            DecodedTurnOut::Expr {
                variant,
                asks,
                wrapped_source,
            } => {
                assert_eq!(variant, 1);
                assert!(asks.is_empty());
                assert!(wrapped_source.contains("1 + 1"));
            }
            other => panic!("expected Expr, got {other:?}"),
        }
    }

    #[test]
    fn decode_turn_out_unknown_tag_is_clean_error() {
        let v = CborValue::Array(vec![
            CborValue::Text("Bogus".into()),
            CborValue::Array(vec![]),
        ]);
        let err = decode_turn_out(&build_cbor(&v)).unwrap_err();
        assert!(
            matches!(err, CompileError::ExtractFailed(_)),
            "expected ExtractFailed, got {err:?}"
        );
    }

    #[test]
    fn decode_turn_out_wrong_arity_is_clean_error() {
        // Bind payload with 4 elements instead of 5 (missing wrappedSource).
        let v = CborValue::Array(vec![
            CborValue::Text("Bind".into()),
            CborValue::Array(vec![
                CborValue::Array(vec![]),
                CborValue::Integer(0.into()),
                CborValue::Array(vec![]),
                CborValue::Array(vec![]),
            ]),
        ]);
        let err = decode_turn_out(&build_cbor(&v)).unwrap_err();
        assert!(
            matches!(err, CompileError::ExtractFailed(_)),
            "expected ExtractFailed, got {err:?}"
        );
    }

    // ---- Part 1: the classification-equivalence corpus (GHC-heavy) ----
    //
    // Every entry's expected `(kind, binders)` is derived from
    // `Tidepool.Binders.classifyTurn`'s documented precedence (its doc
    // comment, not by running the code) — the corpus is a check ON the
    // verdict, not a mirror of whatever the code happens to produce. Each
    // entry asserts the OLD path ([`classify_block`], a direct call) and the
    // NEW path ([`run_turn`]'s returned verdict) agree, and that both match
    // the table.

    /// One corpus entry.
    struct Case {
        name: &'static str,
        text: &'static str,
        kind: TurnKind,
        binders: &'static [&'static str],
    }

    /// classifyTurn rule 1: `<-` / top-level `let` are bind-only markers.
    /// classifyTurn rule 2: a signature (`SigD`) is a decl (resolves the
    /// two-faced `sq :: T`). Rule 3: a name-binding `ValD` is a decl. Rule 4:
    /// a valid bare expression is an expr — runs BEFORE the other-decl
    /// catch-all so `parseDeclaration`'s spurious accept of a bare
    /// application/pipeline as a binder-less splice doesn't misclassify it as
    /// a decl. Rule 5: any other parsed decl (data/class/instance — no
    /// exportable term-level name) is a decl with no binders. Rule 6: neither
    /// parse succeeds → expr, so the real error surfaces at compile.
    const CORPUS: &[Case] = &[
        // -- decl (rules 2/3/5) --
        Case {
            name: "fn_decl",
            text: "sq x = x * x",
            kind: TurnKind::Decl,
            binders: &["sq"],
        },
        Case {
            name: "value_decl",
            text: "x = 5",
            kind: TurnKind::Decl,
            binders: &["x"],
        },
        Case {
            name: "pattern_decl",
            text: "(a, b) = p",
            kind: TurnKind::Decl,
            binders: &["a", "b"],
        },
        Case {
            name: "standalone_signature",
            text: "sq :: Int -> Int",
            kind: TurnKind::Decl,
            binders: &["sq"],
        },
        Case {
            name: "data_decl",
            text: "data Foo = Bar | Baz",
            kind: TurnKind::Decl,
            binders: &[],
        },
        Case {
            name: "class_decl",
            text: "class MyClass a where\n  cls :: a -> a",
            kind: TurnKind::Decl,
            binders: &[],
        },
        Case {
            name: "instance_decl",
            text: "instance Show Foo where\n  show _ = \"foo\"",
            kind: TurnKind::Decl,
            binders: &[],
        },
        // -- decl (rule 3), extension-gated syntax --
        //
        // `run_turn`'s decl branch wraps the turn text in the decl template's
        // 17-extension pragma block before compiling (moved verbatim from
        // the deleted `binders.rs`'s `wrap_decls`). Dropping the wrapper is a
        // compile-boundary narrowing (a valid declaration stops compiling),
        // which is exactly the strict-superset violation the dialect rule
        // forbids. These three are confirmed (by direct probe of
        // `--emit-binders`, wrapped vs. unwrapped) to actually regress
        // without the wrapper: their legality check lives in GHC's
        // lexer/parser, not the renamer, so it fires even under a parse-only
        // extraction. `RecordWildCards`/`GADTs`/`TypeApplications` were also
        // considered — their extension-legality check is deferred to the
        // renamer, a phase the decl parse-only extraction never reaches, so
        // a declaration using them compiles identically wrapped or raw and
        // would NOT catch the wrapper being dropped from this particular
        // decl path; not included here for that reason.
        Case {
            name: "lambda_case_decl",
            text: "f = \\case { 0 -> 1 ; _ -> 2 }",
            kind: TurnKind::Decl,
            binders: &["f"],
        },
        // #321-class regression, decl side (see `quasiquote_bind` below for
        // the bind side): a quasiquote in a *declaration* body must still
        // classify and compile as a decl.
        Case {
            name: "quasiquote_decl",
            text: "greet = [fmt|hello|]",
            kind: TurnKind::Decl,
            binders: &["greet"],
        },
        Case {
            name: "multi_way_if_decl",
            text: "f x = if | x > 0 -> 1 | otherwise -> 2",
            kind: TurnKind::Decl,
            binders: &["f"],
        },
        // -- bind (rule 1) --
        Case {
            name: "monadic_bind",
            text: "x <- pure 1",
            kind: TurnKind::Bind,
            binders: &["x"],
        },
        // Also the `{{TURN_STMT}}` layout case: text begins with `let `.
        Case {
            name: "let_bind",
            text: "let y = 2",
            kind: TurnKind::Bind,
            binders: &["y"],
        },
        Case {
            name: "multi_binder_tuple_bind",
            text: "(a, b) <- pure (1, 2)",
            kind: TurnKind::Bind,
            binders: &["a", "b"],
        },
        // #321-class regression: a quasiquote must still classify as a bind
        // (QuasiQuotes is parse-only here — the quote is one token to the
        // parser).
        Case {
            name: "quasiquote_bind",
            text: "x <- pure [fmt|hi|]",
            kind: TurnKind::Bind,
            binders: &["x"],
        },
        // -- zero-binder bind (Part 3: the discarding shape) --
        Case {
            name: "discard_bind",
            text: "_ <- pure ()",
            kind: TurnKind::Bind,
            binders: &[],
        },
        Case {
            name: "discard_tuple_bind",
            text: "(_, _) <- pure ((), ())",
            kind: TurnKind::Bind,
            binders: &[],
        },
        // -- expr (rule 4: `parseDeclaration`'s spurious decl-shaped accept) --
        Case {
            name: "bare_application",
            text: "id 7",
            kind: TurnKind::Expr,
            binders: &[],
        },
        Case {
            name: "pipeline",
            text: "(+1) . (*2) $ 5",
            kind: TurnKind::Expr,
            binders: &[],
        },
        // -- neither parse succeeds (rule 6) --
        Case {
            name: "unparseable",
            text: "1 +",
            kind: TurnKind::Expr,
            binders: &[],
        },
    ];

    fn binder_names(binders: &[String]) -> Vec<&str> {
        binders.iter().map(String::as_str).collect()
    }

    /// What a DECL case's `TurnResult::Decl` must carry, spelled out per case
    /// from the documented rules rather than read back off a run — the corpus
    /// is a check ON the decl path, so an expectation derived from that path's
    /// own output would assert nothing. `(binders, export-item heads)`:
    ///
    /// - `binders` is `classifyTurn`'s verdict names where it has any (rules
    ///   2/3), and otherwise — rule 5, a declaration with no term-level name —
    ///   the head names the whole-module parse harvested, per `TurnOut`'s
    ///   `TDecl` doc. That fallback is why `data`/`class` report a head here
    ///   while their verdict alone reports nothing.
    /// - heads come from `Tidepool.Binders.declItems`, which reports a `ValD`'s
    ///   binders and a `TyClD`'s head and NOTHING else. So a standalone
    ///   signature harvests no items at all even though `SigD` does yield a
    ///   verdict name, and an `instance` harvests none because it introduces
    ///   no exportable head — the two columns are genuinely independent.
    const DECL_EXPECTATIONS: &[(&str, &[&str], &[&str])] = &[
        ("fn_decl", &["sq"], &["sq"]),
        ("value_decl", &["x"], &["x"]),
        ("pattern_decl", &["a", "b"], &["a", "b"]),
        ("standalone_signature", &["sq"], &[]),
        ("data_decl", &["Foo"], &["Foo"]),
        ("class_decl", &["MyClass"], &["MyClass"]),
        ("instance_decl", &[], &[]),
        ("lambda_case_decl", &["f"], &["f"]),
        ("quasiquote_decl", &["greet"], &["greet"]),
        ("multi_way_if_decl", &["f"], &["f"]),
    ];

    /// Look up a decl case's expectations. A decl case absent from
    /// [`DECL_EXPECTATIONS`] is a hard failure, so adding one to [`CORPUS`]
    /// forces stating what it should produce instead of silently skipping it.
    fn decl_expectations(name: &str) -> (&'static [&'static str], &'static [&'static str]) {
        DECL_EXPECTATIONS
            .iter()
            .find(|(n, _, _)| *n == name)
            .map(|(_, binders, heads)| (*binders, *heads))
            .unwrap_or_else(|| panic!("decl corpus case {name:?} has no DECL_EXPECTATIONS entry"))
    }

    /// Insert an `import Tidepool.QQ (…)` line before the preamble's
    /// `default (…)` decl — the same injection point `tidepool-repl`'s
    /// `insert_imports` uses — so the quasiquote corpus entry has `fmt` in
    /// scope. A no-op fallback (unused import) for every other entry.
    fn with_qq_import(preamble: &str) -> String {
        match preamble.find(tidepool_mcp::PREAMBLE_DEFAULT_DECL) {
            Some(idx) => {
                let mut out = String::with_capacity(preamble.len() + 64);
                out.push_str(&preamble[..idx]);
                out.push_str("import Tidepool.QQ (fmt, j, patch, uri, form)\n");
                out.push_str(&preamble[idx..]);
                out
            }
            None => preamble.to_string(),
        }
    }

    #[test]
    fn turn_classification_corpus_old_and_new_path_agree() {
        if !tidepool_testing::eval_harness::extract_available() {
            eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
            return;
        }

        let decls = tidepool_mcp::standard_decls();
        let effects_dir =
            tidepool_mcp::ensure_effects_module(&decls).expect("write Tidepool.Effects module");
        let prelude_dir = tidepool_testing::eval_harness::prelude_path();
        let preamble = with_qq_import(&tidepool_mcp::build_preamble_non_interactive_mode(
            &decls,
            false,
            tidepool_mcp::PaginateMode::Passthrough,
        ));
        let effect_stack = tidepool_mcp::build_effect_stack_type(&decls);
        let include: Vec<&Path> = vec![&effects_dir, &prelude_dir];

        // A named bind runs the statement, then yields the (comma-joined,
        // parenthesized) binder tuple — valid for both a single name and an
        // N-tuple. A discarding bind runs the statement for its effects and
        // yields `()` — no `{{BINDERS}}` splice, so it needs no bound name at
        // all. An expr places the turn verbatim as the whole binding. A decl
        // selects the parse wrapper (17-extension pragma block).
        let mut bind_source = preamble.clone();
        bind_source.push_str(&format!("__result :: Eff {effect_stack} _\n"));
        bind_source.push_str("__result = do {\n");
        bind_source.push_str("{{TURN_STMT}}");
        bind_source.push_str(" ; pure ({{BINDERS}})\n }\n");

        let mut bind_discard_source = preamble.clone();
        bind_discard_source.push_str(&format!("__result :: Eff {effect_stack} _\n"));
        bind_discard_source.push_str("__result = do {\n");
        bind_discard_source.push_str("{{TURN_STMT}}");
        bind_discard_source.push_str(" ; pure ()\n }\n");

        let mut expr_source = preamble.clone();
        expr_source.push_str("__result = {{TURN}}\n");

        let templates = vec![
            TurnTemplate {
                kind: TemplateSelector::Bind,
                source: bind_source,
            },
            TurnTemplate {
                kind: TemplateSelector::BindDiscard,
                source: bind_discard_source,
            },
            TurnTemplate {
                kind: TemplateSelector::Expr,
                source: expr_source,
            },
            TurnTemplate {
                kind: TemplateSelector::Decl,
                source: DECL_TEMPLATE_SOURCE.to_string(),
            },
        ];

        for case in CORPUS {
            log::debug!("turn_classification_corpus: {}", case.name);
            // Old path: a direct `classify_block` call (single-item batch).
            let old = classify_block(&[case.text])
                .map(|mut v| v.remove(0))
                .unwrap_or_else(|e| panic!("{}: classify_block failed: {e}", case.name));
            assert_eq!(old.kind, case.kind, "{}: old-path kind mismatch", case.name);
            assert_eq!(
                binder_names(&old.binders),
                case.binders,
                "{}: old-path binders mismatch",
                case.name
            );

            // New path: `run_turn`, fed the SAME verdict just obtained (the
            // batch-classify shape) so this exercises template
            // selection/compile, not a second classify spawn.
            let session_root = TempDir::new().unwrap();
            let req = TurnRequest {
                turn_text: case.text,
                templates: &templates,
                include: &include,
                session_root: session_root.path(),
                inject_modules: &[],
                gen: 0,
                verdict: Some(old.clone()),
                target: None,
            };

            match case.name {
                "unparseable" => {
                    // classifyTurn rule 6: both parses failed, so the verdict
                    // is "expr" — but the text isn't valid Haskell, so the
                    // compile itself must fail loudly (the documented
                    // behavior), not silently succeed or panic.
                    let err = run_turn(req).unwrap_err();
                    assert!(
                        matches!(err, CompileError::Diagnostics(_)),
                        "{}: expected a real GHC diagnostic, got {err:?}",
                        case.name
                    );
                }
                _ => {
                    let result = run_turn(req)
                        .unwrap_or_else(|e| panic!("{}: run_turn failed: {e}", case.name));
                    match (case.kind, result) {
                        (TurnKind::Decl, TurnResult::Decl { binders, items }) => {
                            let (want_binders, want_heads) = decl_expectations(case.name);
                            assert_eq!(
                                binder_names(&binders),
                                want_binders,
                                "{}: new-path decl binders mismatch",
                                case.name
                            );
                            assert_eq!(
                                items.iter().map(ExportItem::head_name).collect::<Vec<_>>(),
                                want_heads,
                                "{}: harvested export-item heads mismatch",
                                case.name
                            );
                        }
                        (TurnKind::Bind, TurnResult::Bind { binders, .. }) => {
                            assert_eq!(
                                binder_names(&binders),
                                case.binders,
                                "{}: new-path bind binders mismatch",
                                case.name
                            );
                        }
                        (TurnKind::Expr, TurnResult::Expr { .. }) => {}
                        (kind, other) => panic!(
                            "{}: verdict kind {kind:?} produced the wrong TurnResult variant: {other:?}",
                            case.name
                        ),
                    }
                }
            }
        }
    }
}
