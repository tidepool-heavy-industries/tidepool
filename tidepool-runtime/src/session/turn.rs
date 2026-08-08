//! Wave 3b — session-eval turn compilation (the bind/reference extract seam).
//!
//! A `session_eval` turn is classified by GHC's parser (parse-only) into a BIND
//! (`x <- action` / `let x = e`) or an EXPR (bare expression), then compiled
//! through the session-aware extract path with the live `Tidepool.Session.Val.G<g>`
//! ifaces injected. On a BIND turn the extract also writes the thin session iface
//! (under `session_root`) and emits the [`BoundBinder`] sidecar this module
//! parses.
//!
//! These calls deliberately bypass the memo cache in [`crate::compile_haskell`]:
//! a session turn has on-disk side effects (the iface write) and depends on
//! mutable session state (the injected ifaces), so a cache hit would be wrong.

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

use tidepool_repr::serial::{read_cbor, read_metadata, MetaWarnings};
use tidepool_repr::{CoreExpr, DataConTable};

use crate::{extract_module_name, CompileError};

use super::binders::extract_binders;
use super::render::ExportItem;
use super::SessionError;

/// Strict-force tier of a bound value (mirrors the extract's `BoundBinder.tier`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueTier {
    /// First-order data — `deep_force`d to NF then tenured.
    Tier0Data,
    /// A closure/PAP — tenured as-is (not forced).
    Tier1Closure,
}

/// One binder a BIND turn introduces — the extract's `BoundBinder` JSON record.
#[derive(Clone, Debug)]
pub struct BoundBinder {
    /// The user-facing name (`"x"`).
    pub name: String,
    /// The `0xFE`-tagged stable id minted by `Translate.stableVarId` (carried as
    /// a decimal string in JSON to avoid f64 precision loss).
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
    /// The bound names (GHC-sourced, from [`classify_turn`]). One name for a
    /// single-binder turn; N names for a flat-tuple multi-binder turn.
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
/// selection key template lookup is keyed on. `Decl` never reaches a
/// selector — it doesn't compile through a template (see [`run_turn`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TemplateSelector {
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
    /// Compute the selector a verdict maps to, or `None` for `Decl` (which
    /// never selects a template).
    fn for_verdict(kind: TurnKind, binders: &[String]) -> Option<Self> {
        match kind {
            TurnKind::Decl => None,
            TurnKind::Bind if binders.is_empty() => Some(TemplateSelector::BindDiscard),
            TurnKind::Bind => Some(TemplateSelector::Bind),
            TurnKind::Expr => Some(TemplateSelector::Expr),
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
/// Byte-exactness is the contract: for a given verdict, the module
/// [`run_turn`] compiles must be byte-identical to the module a caller wraps
/// by hand today.
///
/// Several [`TurnTemplate`]s may share a `kind`, forming an ordered variant
/// list (the extract mode's retry: try each in order, first that typechecks
/// wins). [`run_turn`] only ever attempts variant 0 — the list is carried now
/// so the later swap to the extract's own retry needs no signature change.
#[derive(Clone, Debug)]
pub struct TurnTemplate {
    pub kind: TemplateSelector,
    pub source: String,
}

/// One `run_turn` request: the raw turn text, the wrapper templates it may
/// need, the session context [`compile_session_turn`] already takes, the bind
/// generation, and an optional caller-supplied verdict.
///
/// When `verdict` is `Some`, [`run_turn`] uses it verbatim and does not spawn
/// the classify — this is the batch-classify case, where a caller already
/// holds a GHC-sourced verdict for the whole block.
pub struct TurnRequest<'a> {
    /// The raw turn text (`x <- e` / `let x = e` / a bare expression / a
    /// declaration), spliced verbatim into `{{TURN}}`.
    pub turn_text: &'a str,
    /// Wrapper templates, keyed by [`TurnKind`]. Selection is a lookup by the
    /// verdict's kind, never a guess.
    pub templates: &'a [TurnTemplate],
    /// Extra `--include` dirs, forwarded to [`compile_session_turn`] (BIND/EXPR)
    /// or [`extract_binders`] (DECL).
    pub include: &'a [&'a Path],
    /// Where the `Val` ifaces are written/read (BIND/EXPR only).
    pub session_root: &'a Path,
    /// Live `Tidepool.Session.Val.G<g'>` modules to inject (BIND/EXPR only).
    pub inject_modules: &'a [String],
    /// The generation of the `Val.G<g>` module a BIND turn mints.
    pub gen: u64,
    /// A verdict the caller already holds from a batch classify. Skips the
    /// classify spawn when present; GHC-sourced either way.
    pub verdict: Option<TurnClassification>,
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
    /// Typed-yield sites from the `asks.json` sidecar.
    pub asks: Vec<(u32, String)>,
}

/// The result of [`run_turn`] — one variant per verdict, each carrying only
/// its own kind's payload. A caller cannot read a field its verdict does not
/// have.
#[derive(Debug)]
pub enum TurnResult {
    /// A top-level declaration. Does not compile: `items` is the decl's
    /// export items ([`extract_binders`]'s payload).
    Decl {
        /// The declared names (GHC-sourced).
        binders: Vec<String>,
        items: Vec<ExportItem>,
    },
    /// A bind (`x <- e` / `let x = e`).
    Bind {
        /// The verdict's bound/declared names.
        binders: Vec<String>,
        /// The compiled bound-binder records ([`compile_session_turn`]'s
        /// `binders` sidecar payload).
        bound: Vec<BoundBinder>,
        /// Which template variant of the `Bind` kind compiled (always `0` in
        /// this interim body).
        variant: usize,
        compiled: CompiledTurn,
        /// The full wrapped module actually compiled — the byte-identity
        /// anchor.
        wrapped_source: String,
    },
    /// A bare expression.
    Expr {
        /// Which template variant of the `Expr` kind compiled (always `0` in
        /// this interim body).
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
pub fn render_template(source: &str, turn_text: &str, binders: &[String]) -> String {
    let source = source.replace("{{BINDERS}}", &binders.join(", "));
    if source.contains("{{TURN_STMT}}") {
        source.replace("{{TURN_STMT}}", &place_turn_stmt(turn_text))
    } else {
        source.replace("{{TURN}}", turn_text)
    }
}

/// Look up the variant-0 template for `selector` — the first template in
/// declaration order whose `kind` matches. Later same-selector entries are
/// the ordered retry list [`TurnTemplate`] documents; this shim only ever
/// attempts the first.
fn select_template(
    templates: &[TurnTemplate],
    selector: TemplateSelector,
) -> Option<&TurnTemplate> {
    templates.iter().find(|t| t.kind == selector)
}

/// A missing template for a verdict is a caller wiring bug, not a GHC
/// rejection — reported the same way the turn/binder lanes report every other
/// synthetic shape violation (`CompileError::ExtractFailed`, never a panic).
fn missing_template_error(selector: TemplateSelector) -> CompileError {
    CompileError::ExtractFailed(format!("run_turn: no template supplied for {selector:?}"))
}

/// [`SessionError`] → [`CompileError`], preserving the `Io`/`MalformedDiagnostics`
/// split intact (an environment problem stays `Io`, a stale/skewed extractor
/// stays `MalformedDiagnostics`); only the two user-Haskell-shaped variants
/// (`BinderExtraction`, `ValidationFailed`) collapse into `ExtractFailed`,
/// mirroring how this module already reports every other turn-lane synthetic
/// shape violation.
fn session_error_to_compile_error(e: SessionError) -> CompileError {
    match e {
        SessionError::Io(io) => CompileError::Io(io),
        SessionError::MalformedDiagnostics(msg) => CompileError::MalformedDiagnostics(msg),
        SessionError::BinderExtraction(msg) | SessionError::ValidationFailed(msg) => {
            CompileError::ExtractFailed(msg)
        }
    }
}

/// The one entry point for a session-eval turn: classify (unless the caller
/// already supplies a verdict), pick the wrapper template the verdict needs,
/// and compile. For now the body performs today's two (or three, on a DECL
/// turn) spawns — [`classify_turn`], template selection, then
/// [`compile_session_turn`] or [`extract_binders`]. This is scaffolding with
/// a deletion date: when the extract's own `--turn` mode lands, this body
/// becomes a single call and the interim spawns are deleted outright. There
/// is exactly one code path through this function — no flag, no fallback.
pub fn run_turn(req: TurnRequest<'_>) -> Result<TurnResult, CompileError> {
    let TurnClassification { kind, binders } = match req.verdict {
        Some(v) => v,
        None => classify_turn(req.turn_text)?,
    };

    match kind {
        TurnKind::Decl => {
            let items = extract_binders(req.turn_text, req.include)
                .map_err(session_error_to_compile_error)?;
            Ok(TurnResult::Decl { binders, items })
        }
        TurnKind::Bind => {
            // Four-shape selection (protocol note, "the verdict space has
            // four shapes, not three"): a bind that binds no name can't flow
            // through `SessionBind` — the extract rejects an empty
            // `--bind-name` list — so it selects its OWN template and skips
            // `--session-bind` entirely, running as a plain compile that
            // discards its result. `binders`/`bound` both stay empty for
            // that shape, so a caller reading `TurnResult::Bind` sees a
            // uniform "this bind introduced no names" signal either way.
            let selector = TemplateSelector::for_verdict(kind, &binders)
                .expect("TurnKind::Bind always selects Bind or BindDiscard");
            let template = select_template(req.templates, selector)
                .ok_or_else(|| missing_template_error(selector))?;
            let wrapped_source = render_template(&template.source, req.turn_text, &binders);
            let bind = (selector == TemplateSelector::Bind).then(|| SessionBind {
                names: &binders,
                gen: req.gen,
            });
            let result = compile_session_turn(
                &wrapped_source,
                req.include,
                req.session_root,
                req.inject_modules,
                bind,
            )?;
            Ok(TurnResult::Bind {
                binders,
                bound: result.binders,
                variant: 0,
                compiled: CompiledTurn {
                    expr: result.expr,
                    table: result.table,
                    warnings: result.warnings,
                    asks: result.asks,
                },
                wrapped_source,
            })
        }
        TurnKind::Expr => {
            let template = select_template(req.templates, TemplateSelector::Expr)
                .ok_or_else(|| missing_template_error(TemplateSelector::Expr))?;
            let wrapped_source = render_template(&template.source, req.turn_text, &binders);
            let result = compile_session_turn(
                &wrapped_source,
                req.include,
                req.session_root,
                req.inject_modules,
                None,
            )?;
            Ok(TurnResult::Expr {
                variant: 0,
                compiled: CompiledTurn {
                    expr: result.expr,
                    table: result.table,
                    warnings: result.warnings,
                    asks: result.asks,
                },
                wrapped_source,
            })
        }
    }
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
/// parse-only `classify_turn` spawn, `"extract"` (matching
/// `EXTRACT_STAGE_PREFIX`) for a full-pipeline spawn like
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

/// Classify a raw turn (`x <- e` / `let x = e` / a bare expression) via the
/// extract's parse-only `--emit-stmt-binders`. The binder name(s) come from
/// GHC's parser, never a Rust scanner (plan §5.0 / domain §6 R5).
pub fn classify_turn(turn_text: &str) -> Result<TurnClassification, CompileError> {
    let temp = TempDir::new()?;
    let src = temp.path().join("turn.hs");
    std::fs::write(&src, turn_text)?;
    let out = temp.path().join("stmt.json");

    let output = Command::new(extract_bin())
        .arg(&src)
        .arg("--emit-stmt-binders")
        .arg(&out)
        .output()
        .map_err(map_notfound)?;
    // A failed classification still cost a real subprocess spawn — attribute
    // its extract phases the same as a successful one, before the early return
    // below. "classify", not "extract": this is the parse-only lane, a
    // distinct tidepool-extract spawn from the full compile lane.
    forward_extract_timing(&String::from_utf8_lossy(&output.stderr), "classify");
    if !output.status.success() {
        // A parsed report is a real GHC rejection of the turn text; this lane
        // never has a live GHC session distinguishing multiple diagnostics, so
        // joining is realistically a single message, kept as plain
        // `ExtractFailed` (a parse-classification lane, not a
        // compile-diagnostics lane). An UNPARSEABLE report is a stale/skewed
        // extractor — `MalformedDiagnostics` (→ VersionSkew), same as every
        // other extract call site.
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
    let json = std::fs::read_to_string(&out).map_err(CompileError::Io)?;
    parse_stmt_json(&json)
}

fn parse_stmt_json(json: &str) -> Result<TurnClassification, CompileError> {
    let v: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| CompileError::ExtractFailed(format!("invalid stmt-binder JSON: {e}")))?;
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
    let kind = match kind {
        "decl" => TurnKind::Decl,
        "bind" => TurnKind::Bind,
        _ => TurnKind::Expr,
    };
    Ok(TurnClassification { kind, binders })
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
    fn classify_turn_unparseable_report_is_malformed_diagnostics() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let fake = dir.path().join("fake-extract");
        std::fs::write(&fake, "#!/bin/sh\necho not-a-diag-report\nexit 1\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::env::set_var("TIDEPOOL_EXTRACT", &fake);
        let err = classify_turn("x <- pure 1").unwrap_err();
        assert!(
            matches!(err, CompileError::MalformedDiagnostics(_)),
            "expected MalformedDiagnostics, got {err:?}"
        );
    }

    #[test]
    fn parses_bind_classification() {
        let c = parse_stmt_json(r#"{"kind":"bind","binders":["x"]}"#).unwrap();
        assert_eq!(c.kind, TurnKind::Bind);
        assert_eq!(c.binders, vec!["x".to_string()]);
    }

    #[test]
    fn parses_expr_classification() {
        let c = parse_stmt_json(r#"{"kind":"expr","binders":[]}"#).unwrap();
        assert_eq!(c.kind, TurnKind::Expr);
        assert!(c.binders.is_empty());
    }

    #[test]
    fn parses_decl_classification() {
        let c = parse_stmt_json(r#"{"kind":"decl","binders":["sq"]}"#).unwrap();
        assert_eq!(c.kind, TurnKind::Decl);
        assert_eq!(c.binders, vec!["sq".to_string()]);
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
        assert_eq!(TemplateSelector::for_verdict(TurnKind::Decl, &[]), None);
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
        };
        let err = run_turn(req).unwrap_err();
        assert!(
            matches!(err, CompileError::ExtractFailed(_)),
            "expected a clean ExtractFailed, got {err:?}"
        );
    }

    /// A caller-supplied verdict must skip the classify spawn entirely: the
    /// only extractor invocation observed is the compile call, never
    /// `--emit-stmt-binders`. The fake extractor logs its argv and exits
    /// non-zero (the compile is allowed to fail; only the spawn count and
    /// arguments matter here). Env mutation is safe: nextest runs each test
    /// in its own process.
    #[test]
    fn run_turn_supplied_verdict_skips_classify_spawn() {
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
        };
        let _ = run_turn(req);

        let calls = std::fs::read_to_string(&log_path).unwrap_or_default();
        assert_eq!(
            calls.lines().count(),
            1,
            "expected exactly one extract spawn (compile only), got:\n{calls}"
        );
        assert!(
            !calls.contains("--emit-stmt-binders"),
            "classify spawn ran despite a supplied verdict:\n{calls}"
        );
    }

    // ---- Part 1: the classification-equivalence corpus (GHC-heavy) ----
    //
    // Every entry's expected `(kind, binders)` is derived from
    // `Tidepool.Binders.classifyTurn`'s documented precedence (its doc
    // comment, not by running the code) — the corpus is a check ON the
    // verdict, not a mirror of whatever the code happens to produce. Each
    // entry asserts the OLD path (`classify_turn`, a direct call) and the NEW
    // path (`run_turn`'s returned verdict) agree, and that both match the
    // table.

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
        // all (Part 3's fix: it no longer flows an empty name list into
        // `SessionBind`). An expr places the turn verbatim as the whole
        // binding.
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
        ];

        for case in CORPUS {
            log::debug!("turn_classification_corpus: {}", case.name);
            // Old path: a direct `classify_turn` call.
            let old = classify_turn(case.text)
                .unwrap_or_else(|e| panic!("{}: classify_turn failed: {e}", case.name));
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
                        (TurnKind::Decl, TurnResult::Decl { binders, .. }) => {
                            assert_eq!(
                                binder_names(&binders),
                                case.binders,
                                "{}: new-path decl binders mismatch",
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
