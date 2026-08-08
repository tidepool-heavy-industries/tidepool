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

/// A wrapper-module source for one [`TurnKind`], with two substitution
/// points: `{{TURN}}` (the raw turn text, spliced verbatim) and `{{BINDERS}}`
/// (the verdict's binder names, comma-joined). Byte-exactness is the
/// contract: for a given verdict, the module [`run_turn`] compiles must be
/// byte-identical to the module a caller wraps by hand today.
///
/// Several [`TurnTemplate`]s may share a `kind`, forming an ordered variant
/// list (the extract mode's retry: try each in order, first that typechecks
/// wins). [`run_turn`] only ever attempts variant 0 — the list is carried now
/// so the later swap to the extract's own retry needs no signature change.
#[derive(Clone, Debug)]
pub struct TurnTemplate {
    pub kind: TurnKind,
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

/// Splice `turn_text` and `binders` into a template `source`. `{{BINDERS}}`
/// is substituted first, `{{TURN}}` last, so turn text containing literal
/// `{{BINDERS}}`/`{{TURN}}` text is never re-scanned — the turn splice is
/// byte-exact regardless of its content.
fn render_template(source: &str, turn_text: &str, binders: &[String]) -> String {
    source
        .replace("{{BINDERS}}", &binders.join(", "))
        .replace("{{TURN}}", turn_text)
}

/// Look up the variant-0 template for `kind` — the first template in
/// declaration order whose `kind` matches. Later same-kind entries are the
/// ordered retry list [`TurnTemplate`] documents; this shim only ever
/// attempts the first.
fn select_template(templates: &[TurnTemplate], kind: TurnKind) -> Option<&TurnTemplate> {
    templates.iter().find(|t| t.kind == kind)
}

/// A missing template for a verdict is a caller wiring bug, not a GHC
/// rejection — reported the same way the turn/binder lanes report every other
/// synthetic shape violation (`CompileError::ExtractFailed`, never a panic).
fn missing_template_error(kind: TurnKind) -> CompileError {
    CompileError::ExtractFailed(format!("run_turn: no template supplied for {kind:?}"))
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
            let template = select_template(req.templates, TurnKind::Bind)
                .ok_or_else(|| missing_template_error(TurnKind::Bind))?;
            let wrapped_source = render_template(&template.source, req.turn_text, &binders);
            let bind = SessionBind {
                names: &binders,
                gen: req.gen,
            };
            let result = compile_session_turn(
                &wrapped_source,
                req.include,
                req.session_root,
                req.inject_modules,
                Some(bind),
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
            let template = select_template(req.templates, TurnKind::Expr)
                .ok_or_else(|| missing_template_error(TurnKind::Expr))?;
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
                kind: TurnKind::Bind,
                source: "bind-a".to_string(),
            },
            TurnTemplate {
                kind: TurnKind::Expr,
                source: "expr-a".to_string(),
            },
            TurnTemplate {
                kind: TurnKind::Bind,
                source: "bind-b".to_string(),
            },
        ];
        assert_eq!(
            select_template(&templates, TurnKind::Bind).unwrap().source,
            "bind-a"
        );
        assert_eq!(
            select_template(&templates, TurnKind::Expr).unwrap().source,
            "expr-a"
        );
        assert!(select_template(&templates, TurnKind::Decl).is_none());
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
            kind: TurnKind::Expr,
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
}
