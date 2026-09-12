//! Session-eval turn compilation (the bind/reference extract seam).
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

use ciborium::value::Value as CborValue;
use tempfile::TempDir;
use tidepool_extract_cmd::{ExtractCmd, SpawnError};
use tidepool_toolchain::extract_module_name;

use tidepool_repr::serial::{read_cbor, read_metadata, MetaWarnings};
use tidepool_repr::{CoreExpr, DataConTable};

use crate::{timing, CompileError, NominalHead, SiteType, YieldSite};

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
    /// Structured declaration exports from the same GHC parse. Empty for
    /// bind/expr turns and for declarations such as signatures or instances
    /// that introduce no export by themselves.
    pub items: Vec<ExportItem>,
}

/// One-based source coordinates reported by GHC for a notebook-cell item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellSourceSpan {
    pub start_line: usize,
    pub start_column: usize,
    pub end_line: usize,
    pub end_column: usize,
}

/// One item classified during whole-cell preflight.
#[derive(Clone, Debug)]
pub struct CellAnalysisItem {
    pub span: CellSourceSpan,
    pub source: String,
    pub verdict: TurnClassification,
}

/// A post-zonk statement-binder type inferred by the whole-cell check.
#[derive(Clone, Debug)]
pub struct CheckedBinderPin {
    /// Compiler-reserved alias key. It encodes the item index and binder name
    /// but is never parsed for control flow by the compiler worker.
    pub key: String,
    /// GHC-rendered type replanted into the staged bind wrapper.
    pub ty: String,
    /// Nominal heads used by relocation/preflight to identify same-cell names.
    pub heads: Vec<NominalHead>,
}

/// Successful whole-cell preflight result.
#[derive(Clone, Debug)]
pub struct CellCheck {
    pub items: Vec<CellAnalysisItem>,
    pub pins: Vec<CheckedBinderPin>,
    /// Exact generated module GHC checked.
    pub checked_source: String,
}

impl CellCheck {
    /// Resolve pins for one classified bind without parsing compiler output.
    /// Expected keys are derived from the source item and GHC-reported binder
    /// names; missing or duplicate pins reject preflight.
    pub fn pins_for_item(&self, item_index: usize) -> Result<Vec<CheckedBinderPin>, CompileError> {
        let item = self.items.get(item_index).ok_or_else(|| {
            CompileError::ExtractFailed(format!("cell item index {item_index} is out of range"))
        })?;
        if item.verdict.kind != TurnKind::Bind {
            return Ok(Vec::new());
        }
        item.verdict
            .binders
            .iter()
            .map(|binder| {
                let key = format!("__tidepool_cell_pin_{item_index}_{binder}");
                let mut matches = self.pins.iter().filter(|pin| pin.key == key);
                let pin = matches.next().cloned().ok_or_else(|| {
                    CompileError::ExtractFailed(format!(
                        "whole-cell check returned no type for binder {binder:?} in item {}",
                        item_index + 1
                    ))
                })?;
                if matches.next().is_some() {
                    return Err(CompileError::ExtractFailed(format!(
                        "whole-cell check returned duplicate type pins for {binder:?} in item {}",
                        item_index + 1
                    )));
                }
                Ok(pin)
            })
            .collect()
    }
}

/// Runtime-owned inputs to the compiler worker's whole-cell request.
pub struct CellCheckRequest<'a> {
    pub cell_text: &'a str,
    pub template: &'a str,
    pub include: &'a [&'a Path],
    pub session_root: &'a Path,
    pub inject_modules: &'a [String],
}

/// Which wrapper template a verdict selects. A refinement of [`TurnKind`]:
/// a `Bind` verdict maps to one of two distinct template shapes depending on
/// whether it actually binds a name (`binders.is_empty()`) — a discarding
/// bind (`_ <- e`) has no value to install, so it needs its own wrapper.
/// [`TurnKind`] stays a plain 3-value mirror of the extract's wire-contract
/// `kind` string ([`Decl`]/[`Bind`]/[`Expr`](TurnKind)); this is the separate,
/// Rust-only selection key used for template lookup. `Decl` also selects its
/// parse wrapper but never compiles through it.
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
/// wrapper, authored once and spliced via `{{TURN}}`.
///
/// The pragma block is deliberately a SUBSET of the canonical eval dialect
/// ([`super::EVAL_PRAGMAS`]): this pass PARSES but never
/// typechecks or renames, so extensions that only affect type inference,
/// instance resolution, or scoping have no business here. That delta is
/// declared and enforced — `tidepool-mcp`'s `pragma_set_consistency` test
/// asserts an exact subset with the excluded set spelled out, so adding an
/// extension here that eval does not also carry fails loud rather than
/// silently parsing session-decl code in a different dialect than eval does.
pub const DECL_TEMPLATE_SOURCE: &str = "{-# LANGUAGE GADTs, OverloadedStrings, TypeOperators, DataKinds, KindSignatures, ScopedTypeVariables, BangPatterns, ViewPatterns, TupleSections, MultiWayIf, LambdaCase, RecordWildCards, NamedFieldPuns, DeriveFunctor, DeriveFoldable, DeriveTraversable, TypeApplications, QuasiQuotes, OverloadedLabels #-}\nmodule SessionDecls where\n{{TURN}}\n";

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

/// Failure from the shared resident-turn boundary.
///
/// `attempted_source` is present when the extractor reached a rendered
/// template and failed while compiling it. Frontends that remap GHC spans use
/// this exact source; other callers can discard it without reconstructing the
/// worker's template choice.
#[derive(Debug)]
pub struct TurnFailure {
    pub error: CompileError,
    pub attempted_source: Option<String>,
}

impl std::fmt::Display for TurnFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}

impl std::error::Error for TurnFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

impl From<CompileError> for TurnFailure {
    fn from(error: CompileError) -> Self {
        Self {
            error,
            attempted_source: None,
        }
    }
}

impl From<std::io::Error> for TurnFailure {
    fn from(error: std::io::Error) -> Self {
        CompileError::from(error).into()
    }
}

/// Render a failed resident turn against the submitted input unit rather than
/// the generated wrapper module. Diagnostics from other files retain their
/// original coordinates.
#[must_use]
pub fn render_turn_compile_error(
    error: &CompileError,
    attempted_source: Option<&str>,
    turn_text: &str,
    label: &str,
) -> String {
    let CompileError::Diagnostics(diagnostics) = error else {
        return crate::classify_compile(error).message;
    };
    let Some(source) = attempted_source else {
        return crate::classify_compile(error).message;
    };
    let anchor = extract_module_name(source)
        .map(|module| format!("{}.hs", module.replace('.', "/")))
        .unwrap_or_else(|| "Expr.hs".into());
    let (line_offset, col_indent) = turn_user_code_offset(source).unwrap_or((0, 0));
    let user_lines = turn_user_code_line_range(source, turn_text);
    crate::diag::render_diagnostics(
        diagnostics,
        &crate::diag::RenderOpts {
            anchor: &anchor,
            label,
            user_lines: user_lines.as_ref().map(std::slice::from_ref),
            line_offset,
            col_indent,
            drop_foreign_gen_warnings_except: None,
            source,
        },
    )
}

/// Locate submitted turn text within one of the shared wrapper templates.
#[must_use]
pub fn turn_user_code_offset(source: &str) -> Option<(usize, usize)> {
    const GUARDED_PURE_MARKER: &str =
        "__workbenchValue = let {\n __value = __tidepoolPureWorkbenchValue $ ";
    if let Some(position) = source.find(GUARDED_PURE_MARKER) {
        let prefix = &source[..position + GUARDED_PURE_MARKER.len()];
        let indent = prefix
            .rsplit_once('\n')
            .map_or(prefix.len(), |(_, line)| line.len());
        return Some((prefix.matches('\n').count(), indent));
    }
    for marker in [
        "__user = let {\n __b =\n",
        "__probe = let {\n __b =\n",
        "__workbenchValue = let {\n __value =\n",
        "__workbenchValue = __tidepoolInEffectRow $ let {\n __value =\n",
    ] {
        if let Some(position) = source.find(marker) {
            return Some((source[..position + marker.len()].matches('\n').count(), 0));
        }
    }
    const DECL_MODULE: &str = "module SessionDecls where\n";
    if let Some(position) = source.find(DECL_MODULE) {
        return Some((
            source[..position + DECL_MODULE.len()].matches('\n').count(),
            0,
        ));
    }
    const RESULT_DO: &str = "\n__result = do {\n";
    source.find(RESULT_DO).map(|position| {
        (
            source[..position + RESULT_DO.len()].matches('\n').count(),
            0,
        )
    })
}

/// Inclusive generated-module line range occupied by the submitted turn.
#[must_use]
pub fn turn_user_code_line_range(wrapped: &str, turn_text: &str) -> Option<(usize, usize)> {
    let (offset, _) = turn_user_code_offset(wrapped)?;
    let lines = if turn_text.is_empty() {
        1
    } else if turn_text.ends_with('\n') {
        turn_text.matches('\n').count()
    } else {
        turn_text.matches('\n').count() + 1
    };
    let start = offset + 1;
    Some((start, start + lines - 1))
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
    /// Typed suspension sites decoded from the `TurnOut` wire payload.
    pub asks: Vec<YieldSite>,
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
    Decl(DeclarationReceipt),
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

/// GHC's complete parse-only receipt for one declaration turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclarationReceipt {
    /// Declared names from the verdict/whole-module parse.
    pub binders: Vec<String>,
    /// Exported values, types, and classes, including constructor/method facts.
    pub items: Vec<ExportItem>,
}

/// The preamble's `default (...)` declaration line — the same import
/// injection point `tidepool_mcp::template_haskell` uses. MUST match
/// `tidepool_mcp::PREAMBLE_DEFAULT_DECL` byte-for-byte; duplicated as a
/// literal (rather than depending on it) because `tidepool-mcp` depends on
/// `tidepool-runtime`, not the other way — a real (non-dev) dependency here
/// would be circular.
const PREAMBLE_DEFAULT_MARKER: &str = "default (Int, Double, Text)\n";

/// Insert `import <m>` lines into `preamble` immediately before its
/// [`PREAMBLE_DEFAULT_MARKER`] line — the same injection point
/// `template_haskell` uses. A no-op (returns `preamble` unchanged) when
/// `imports` is blank. The ONE import-insertion mechanism a session-turn
/// module builder needs — `tidepool-repl`'s and `tidepool-harness`'s own turn
/// wrappers call this rather than reimplementing the same marker search.
pub fn insert_preamble_imports(preamble: &str, imports: &str) -> String {
    if imports.trim().is_empty() {
        return preamble.to_string();
    }
    let insert_point = preamble
        .find(PREAMBLE_DEFAULT_MARKER)
        .unwrap_or(preamble.len());
    let mut out = String::new();
    out.push_str(&preamble[..insert_point]);
    for imp in imports.lines().map(str::trim).filter(|l| !l.is_empty()) {
        out.push_str("import ");
        out.push_str(imp);
        out.push('\n');
    }
    out.push_str(&preamble[insert_point..]);
    out
}

/// Enable generalization for compiler-only probes. Session declaration
/// modules already use this rule; inspection and pure-reference probes must
/// do the same or an imported polymorphic value can acquire a misleading
/// monomorphic type merely because it was mentioned by the probe.
#[must_use]
pub fn enable_no_monomorphism_restriction(preamble: &str) -> String {
    if preamble.contains("NoMonomorphismRestriction") {
        return preamble.to_string();
    }
    preamble.replacen(
        "NoImplicitPrelude,",
        "NoImplicitPrelude, NoMonomorphismRestriction,",
        1,
    )
}

/// Assemble the compiler-only module used by `:type` and `:info`. A type
/// query binds the expression through an explicit-brace `let`, preserving
/// arbitrary user layout and quasiquote bytes; an info query needs only a
/// target-module anchor so GHC produces its resolved reader environment.
pub fn assemble_inspection_module(preamble: &str, imports: &str, expressions: &[String]) -> String {
    let preamble = enable_no_monomorphism_restriction(preamble);
    let mut source = insert_preamble_imports(&preamble, imports);
    if expressions.is_empty() {
        source
            .push_str("\n__tidepool_inspection_anchor :: ()\n__tidepool_inspection_anchor = ()\n");
    } else {
        for (index, expression) in expressions.iter().enumerate() {
            source.push_str(&format!("\n__tidepool_inspect_{index} = let {{\n __b =\n"));
            source.push_str(expression);
            if !expression.ends_with('\n') {
                source.push('\n');
            }
            source.push_str(" } in __b\n");
        }
    }
    source
}

/// Assemble a "bind-shaped" session-turn module by concatenating, in order:
/// `preamble_with_imports` (imports already inserted — see
/// [`insert_preamble_imports`]), the `-- [user]\n` marker, caller-supplied
/// `extra` (a value binding, helper decls, or nothing), the `<target> = do {
/// ... }` binding, `stmt`
/// (already placement-normalized via [`place_turn_stmt`], or a literal
/// `{{TURN_STMT}}` marker for a caller building a [`TurnTemplate`]), and the
/// ` ; pure <tail> }` closer — optionally wrapped in `runDelegate( ... )` at
/// the result position for a delegate-scoped turn.
///
/// This is the ONE mechanism behind every BIND/BINDDISCARD/MULTIBIND session
/// wrapper in both `tidepool-repl` (`wrap_bind_source`/
/// `wrap_bind_discard_source`/`wrap_multi_bind_source`) and `tidepool-harness`
/// (`template_session_bind`/`session_bind_template`) — those stay as each
/// crate's own thin, policy-only callers (what `extra`/`tail`/`delegate_wrap`
/// to pass), not a second copy of this assembly.
pub fn assemble_bind_module(
    preamble_with_imports: &str,
    extra: &str,
    target: &str,
    effect_stack: &str,
    stmt: &str,
    tail: &str,
    delegate_wrap: bool,
) -> String {
    let mut out = preamble_with_imports.to_string();
    out.push_str("-- [user]\n");
    out.push_str(extra);
    if delegate_wrap {
        out.push_str(&format!(
            "__tidepoolInEffectRow :: Eff {effect_stack} value -> Eff {effect_stack} value\n\
             __tidepoolInEffectRow = id\n\
             {target} = __tidepoolInEffectRow (runDelegate (do {{\n"
        ));
    } else {
        out.push_str(&format!("{target} = do {{\n"));
    }
    out.push_str(stmt);
    if delegate_wrap {
        out.push_str(&format!(" ; pure {tail}\n }}))\n"));
    } else {
        // Pin only the effect row. Let GHC infer the result type so generated
        // workbench scaffolding cannot manufacture a partial-signature warning.
        out.push_str(&format!(
            " ; _ <- (pure () :: Eff {effect_stack} ())\n ; pure {tail}\n }}\n"
        ));
    }
    out
}

/// How a resident expression is lifted into the actor/workbench effect row.
/// Keeping both candidates as ordered templates lets GHC decide whether the
/// expression is already effectful; Rust does not guess from syntax.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExpressionLift {
    Effectful,
    Pure,
}

/// Assemble one expression-shaped session module while preserving the source
/// text byte-for-byte inside an explicit-layout binding. The caller normally
/// supplies two templates—[`ExpressionLift::Effectful`] then
/// [`ExpressionLift::Pure`]—and the turn extractor selects the first one that
/// typechecks.
pub fn assemble_expression_module(
    preamble_with_imports: &str,
    target: &str,
    effect_stack: &str,
    expression: &str,
    lift: ExpressionLift,
) -> String {
    assemble_expression_module_with_result(
        preamble_with_imports,
        target,
        effect_stack,
        expression,
        lift,
        ExpressionResult::Raw,
    )
}

#[derive(Clone, Copy)]
enum ExpressionResult {
    Raw,
    HaskellDisplay,
    OpaqueDisplay,
    Observation,
}

fn assemble_expression_module_with_result(
    preamble_with_imports: &str,
    target: &str,
    effect_stack: &str,
    expression: &str,
    lift: ExpressionLift,
    result: ExpressionResult,
) -> String {
    let mut out = if matches!(lift, ExpressionLift::Pure) {
        insert_preamble_imports(
            preamble_with_imports,
            "qualified GHC.TypeError as TidepoolWorkbenchTypeError",
        )
    } else {
        preamble_with_imports.to_string()
    };
    out.push_str(&format!(
        "__tidepoolInEffectRow :: Eff {effect_stack} value -> Eff {effect_stack} value\n\
         __tidepoolInEffectRow = id\n"
    ));
    if matches!(lift, ExpressionLift::Pure) {
        out.push_str(concat!(
            "\nclass TidepoolPureWorkbenchValue value\n",
            "instance {-# OVERLAPPABLE #-} TidepoolPureWorkbenchValue value\n",
            "instance {-# OVERLAPPING #-} TidepoolWorkbenchTypeError.Unsatisfiable ",
            "('TidepoolWorkbenchTypeError.Text \"an Eff action must typecheck in the current workbench effect row\") ",
            "=> TidepoolPureWorkbenchValue (Eff effects value)\n",
            "__tidepoolPureWorkbenchValue :: TidepoolPureWorkbenchValue value => value -> value\n",
            "__tidepoolPureWorkbenchValue = id\n",
        ));
    }
    out.push_str("-- [user]\n");
    if matches!(lift, ExpressionLift::Effectful) {
        // Give GHC the actor's exact row while it infers the user expression.
        // Without this pin, the let-bound expression is
        // generalized before `__result` constrains it; polymorphic `Member`
        // actions then lose the exact workbench row needed for inference.
        out.push_str("__workbenchValue = __tidepoolInEffectRow $ let {\n __value =\n");
    }
    if matches!(lift, ExpressionLift::Pure) {
        out.push_str("__workbenchValue = let {\n __value = __tidepoolPureWorkbenchValue $ ");
    }
    out.push_str(expression);
    if !expression.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(" } in __value\n");
    let body = match (lift, result) {
        (ExpressionLift::Effectful, ExpressionResult::Raw) => "__workbenchValue",
        (ExpressionLift::Pure, ExpressionResult::Raw) => "pure __workbenchValue",
        (ExpressionLift::Effectful, ExpressionResult::HaskellDisplay) => {
            "do { __value <- __workbenchValue ; pure (__value, T.pack (show __value)) }"
        }
        (ExpressionLift::Pure, ExpressionResult::HaskellDisplay) => {
            "pure (__workbenchValue, T.pack (show __workbenchValue))"
        }
        (ExpressionLift::Effectful, ExpressionResult::OpaqueDisplay) => {
            "do { _ <- __workbenchValue ; pure (T.pack \"<opaque value>\") }"
        }
        (ExpressionLift::Pure, ExpressionResult::OpaqueDisplay) => {
            "pure (__workbenchValue `seq` T.pack \"<opaque value>\")"
        }
        (ExpressionLift::Effectful, ExpressionResult::Observation) => {
            "do { __value <- __workbenchValue ; pure (\\() -> __value) }"
        }
        (ExpressionLift::Pure, ExpressionResult::Observation) => "pure (\\() -> __workbenchValue)",
    };
    out.push_str(target);
    out.push_str(" = __tidepoolInEffectRow $ ");
    out.push_str(body);
    out.push('\n');
    out
}

/// Capture one bare observation as a typed thunk. Effects run once, while
/// retaining the result does not deep-force an arbitrary payload or its Show.
pub fn assemble_observation_module(
    preamble: &str,
    target: &str,
    effect_stack: &str,
    expression: &str,
    lift: ExpressionLift,
) -> String {
    assemble_expression_module_with_result(
        preamble,
        target,
        effect_stack,
        expression,
        lift,
        ExpressionResult::Observation,
    )
}

/// Assemble the display-capable sibling of [`assemble_expression_module`].
/// The authored expression runs once and returns both its original value and
/// its Haskell rendering, so Rust never needs to interpret a Haskell value or
/// re-run an effect merely to print its result.
pub fn assemble_display_expression_module(
    preamble_with_imports: &str,
    target: &str,
    effect_stack: &str,
    expression: &str,
    lift: ExpressionLift,
) -> String {
    assemble_expression_module_with_result(
        preamble_with_imports,
        target,
        effect_stack,
        expression,
        lift,
        ExpressionResult::HaskellDisplay,
    )
}

/// Assemble an expression fallback for values with no rendering instance.
/// Effectful values still run exactly once; pure values are evaluated to weak
/// head normal form. Only the fixed display token crosses into Rust, so
/// closures and opaque references never enter the value serializer.
pub fn assemble_opaque_expression_module(
    preamble_with_imports: &str,
    target: &str,
    effect_stack: &str,
    expression: &str,
    lift: ExpressionLift,
) -> String {
    assemble_expression_module_with_result(
        preamble_with_imports,
        target,
        effect_stack,
        expression,
        lift,
        ExpressionResult::OpaqueDisplay,
    )
}

/// Place `turn_text` as a `do`-block statement — the `{{TURN_STMT}}`
/// placement mode. Mirrors `tidepool-repl`'s and `tidepool-harness`'s own
/// `push_braced_stmt` wrappers (both now thin callers of this function): a
/// `let` turn (at column 1, since a raw turn has no leading indentation)
/// needs explicit decl braces there (a layout `let` swallows the following
/// `;`), so it is rewritten to `let { <rest> }`; anything else is placed
/// verbatim. Both branches guarantee a trailing newline so a template's own
/// following text always starts on a fresh line.
pub fn place_turn_stmt(turn_text: &str) -> String {
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

/// A fresh [`ExtractCmd`] with this crate's error mapping already applied: a
/// misconfigured `$TIDEPOOL_EXTRACT` is an environment problem, reported the
/// same way a failed spawn is (`Io`, never a user-Haskell variant).
///
/// Also carries the default build-products dir
/// (`crate::paths::apply_build_products_dir`) — this module's spawns bypass
/// `crate::artifacts::compile_invocation`'s memo (see the module doc: a
/// session turn has on-disk side effects and mutable-session dependencies a
/// content-addressed cache would get wrong), but the module-granular GHC
/// recompilation win the build-products dir gives is an orthogonal, additive
/// concern, and this crate's OTHER (turn/eval) spawns get it too — this is
/// what makes it on-by-default here rather than only through that one lane.
fn extract_cmd() -> Result<ExtractCmd, CompileError> {
    ExtractCmd::new().map_err(|e| CompileError::Io(e.into()))
}

fn map_notfound(e: SpawnError) -> CompileError {
    CompileError::Io(crate::extract_spawn_error(e.source))
}

/// Parse `stderr` via [`timing::ExtractTiming::parse`] and re-emit each phase
/// as a `<prefix>.<phase>` stage via [`timing::record_stage`].
///
/// `prefix` selects which `tidepool-extract` spawn these phases came from —
/// pass `"classify"` (matching [`timing::CLASSIFY_STAGE_PREFIX`]) for the
/// parse-only [`classify_block`] spawn, `"extract"` (matching
/// [`timing::EXTRACT_STAGE_PREFIX`]) for a full-pipeline spawn like
/// [`run_turn`]'s. Different subprocess spawns must never share a prefix: a
/// collector summing by stage name would silently merge their costs.
fn forward_extract_timing(stderr: &str, prefix: &str) {
    let stage_name = |phase: &str| match prefix {
        "extract" => timing::extract_stage_name(phase),
        "classify" => timing::classify_stage_name(phase),
        other => unreachable!("forward_extract_timing: unknown prefix {other:?}"),
    };
    let parsed = timing::ExtractTiming::parse(stderr);
    for (phase, ms) in &parsed.phases {
        timing::record_stage(
            timing::NO_NODE,
            timing::NO_ROUND,
            &stage_name(phase),
            std::time::Duration::from_millis(*ms),
            0,
        );
    }
}

/// Run GHC's split/classify/whole-cell check before any declaration or user
/// effect is installed. The runtime supplies the exact next-cell scope as a
/// source template; the worker owns Haskell parsing and post-zonk binder
/// harvesting.
pub fn check_cell(req: CellCheckRequest<'_>) -> Result<CellCheck, CompileError> {
    let temp = TempDir::new()?;
    let cell_path = temp.path().join("cell.txt");
    let template_path = temp.path().join("CellCheckTemplate.hs");
    let out_path = temp.path().join("cell.cbor");
    std::fs::write(&cell_path, req.cell_text)?;
    std::fs::write(&template_path, req.template)?;

    let mut cmd = extract_cmd()?;
    cmd.input(&cell_path)
        .cell()
        .cell_template(&template_path)
        .cell_out(&out_path)
        .output_dir(temp.path())
        .includes(req.include)
        .session_root(req.session_root)
        .inject_vals(req.inject_modules);
    let endpoint = cmd.bind().map_err(map_notfound)?;
    crate::paths::apply_build_products_dir(&mut cmd, &endpoint);
    let run = endpoint.execute(&cmd).map_err(map_notfound)?;
    let output = &run.output;
    if let Err(error) =
        crate::diag::decode_extract_result(run.success(), &output.stdout, &output.stderr)
    {
        return Err(error);
    }
    let bytes = std::fs::read(&out_path)?;
    decode_cell_out(&bytes)
}

/// The one entry point for a session-eval turn. Writes the turn text and
/// every supplied wrapper template to a [`TempDir`], then performs exactly
/// ONE `tidepool-extract --turn` spawn: `--turn-template <kind>=<path>` per
/// template (in order), `--turn-out`/`--output-dir` into the same temp dir,
/// `--include`/`--session-root`/`--inject-val`/`--bind-gen`, and
/// `--turn-verdict` when `req.verdict` is supplied (skipping the extract's
/// internal re-parse). Decodes the `TurnOut` CBOR sidecar into the matching
/// [`TurnResult`] variant; for `Bind`/`Expr` also reads `result.cbor` /
/// `meta.cbor` from that same output directory into [`CompiledTurn`].
///
/// When `req.verdict` is supplied, a missing template for the verdict's
/// selector is caught here, before any process is spawned.
pub fn run_turn(req: TurnRequest<'_>) -> Result<TurnResult, TurnFailure> {
    run_turn_with_pin(req, None)
}

/// Compile one staged bind using the types inferred by 'check_cell'. This is
/// the join that prevents statement-at-a-time compilation from defaulting or
/// generalizing a binder differently from the accepted whole cell.
pub fn run_turn_pinned(
    req: TurnRequest<'_>,
    pins: &[CheckedBinderPin],
) -> Result<TurnResult, TurnFailure> {
    let Some(TurnClassification {
        kind: TurnKind::Bind,
        binders,
        ..
    }) = req.verdict.as_ref()
    else {
        return Err(CompileError::ExtractFailed(
            "checked binder pins require an explicit bind verdict".into(),
        )
        .into());
    };
    if pins.len() != binders.len() {
        return Err(CompileError::ExtractFailed(format!(
            "checked binder pin count {} does not match binder count {}",
            pins.len(),
            binders.len()
        ))
        .into());
    }
    let pin = if binders.len() == 1 {
        format!("{} :: {}", binders[0], pins[0].ty)
    } else {
        format!(
            "({}) :: ({})",
            binders.join(", "),
            pins.iter()
                .map(|pin| pin.ty.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    run_turn_with_pin(req, Some(&pin))
}

fn run_turn_with_pin(req: TurnRequest<'_>, pin: Option<&str>) -> Result<TurnResult, TurnFailure> {
    let verdict_arg = match &req.verdict {
        Some(TurnClassification { kind, binders, .. }) => {
            #[allow(
                clippy::expect_used,
                reason = "TemplateSelector::for_verdict is total over TurnKind"
            )]
            let selector = TemplateSelector::for_verdict(*kind, binders)
                .expect("TemplateSelector::for_verdict is total over TurnKind");
            if select_template(req.templates, selector).is_none() {
                return Err(missing_template_error(selector).into());
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

    let mut cmd = extract_cmd()?;
    cmd.input(&turn_path).turn();

    for (i, tmpl) in req.templates.iter().enumerate() {
        let path = temp.path().join(format!("template-{i}.hs"));
        std::fs::write(&path, &tmpl.source)?;
        cmd.turn_template(tmpl.kind.wire_name(), &path);
    }

    cmd.turn_out(&turn_out_path)
        .output_dir(temp.path())
        .includes(req.include)
        .session_root(req.session_root)
        .inject_vals(req.inject_modules)
        .bind_gen(req.gen);
    if let Some(target) = req.target {
        cmd.target(target);
    }
    if let Some(arg) = verdict_arg {
        cmd.turn_verdict(arg);
    }
    if let Some(pin) = pin {
        cmd.turn_pin(pin);
    }

    let endpoint = cmd.bind().map_err(map_notfound)?;
    crate::paths::apply_build_products_dir(&mut cmd, &endpoint);
    let run = endpoint.execute(&cmd).map_err(map_notfound)?;
    timing::record_stage(
        timing::NO_NODE,
        timing::NO_ROUND,
        timing::STAGE_EXTRACT_SPAWN,
        run.elapsed,
        0,
    );
    let output = &run.output;
    let stderr = run.stderr_lossy();
    // A failed compile is still a real spawn — attribute its extract phases
    // the same as a successful one, before the early return below.
    forward_extract_timing(&stderr, "extract");
    // Default-on per-compile summary + gated per-module breakdown
    // (compile-attribution lane): this `--turn` spawn goes through the SAME
    // `Tidepool.GhcPipeline.runCompile` skeleton `artifacts.rs::extract_and_read`
    // instruments (`runTurnMode` → `runPipelineSession` → `runCompile` — see
    // `haskell/app/Main.hs`), but reads its own `stderr` here rather than
    // through that shared function, so it needs the same two calls duplicated
    // rather than silently missing them.
    if let Some(summary) = timing::CompileSummary::parse(&stderr) {
        timing::log_compile_summary(&summary);
    }
    let module_timings = timing::parse_module_timings(&stderr);
    if !module_timings.is_empty() {
        timing::log_module_timings(&module_timings);
    }
    if let Err(error) =
        crate::diag::decode_extract_result(run.success(), &output.stdout, &output.stderr)
    {
        let attempted_source = std::fs::read_to_string(temp.path().join("turn-attempt.hs")).ok();
        return Err(TurnFailure {
            error,
            attempted_source,
        });
    }

    decode_turn_output_dir(temp.path()).map_err(Into::into)
}

/// Decode one item's full output directory into a [`TurnResult`]: the
/// `TurnOut` CBOR sidecar (`turn.cbor`) plus, for a `Bind`/`Expr` verdict,
/// `result.cbor`/`meta.cbor` off the SAME directory.
fn decode_turn_output_dir(dir: &Path) -> Result<TurnResult, CompileError> {
    let turn_out_path = dir.join("turn.cbor");
    if !turn_out_path.exists() {
        return Err(CompileError::MissingOutput(turn_out_path));
    }
    let turn_out_bytes = std::fs::read(&turn_out_path)?;
    let turn_out = decode_turn_out(&turn_out_bytes)?;

    match turn_out {
        DecodedTurnOut::Decl { binders, items } => {
            Ok(TurnResult::Decl(DeclarationReceipt { binders, items }))
        }
        DecodedTurnOut::Bind {
            binders,
            variant,
            bound,
            asks,
            wrapped_source,
        } => {
            let compiled = read_compiled_turn(dir, asks)?;
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
            let compiled = read_compiled_turn(dir, asks)?;
            Ok(TurnResult::Expr {
                variant,
                compiled,
                wrapped_source,
            })
        }
    }
}

/// Read one compiled artifact and register its warning metadata. `asks` comes
/// from the already-decoded turn result, not a parallel sidecar.
fn read_compiled_turn(
    output_dir: &Path,
    asks: Vec<YieldSite>,
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
    timing::record_stage(
        timing::NO_NODE,
        timing::NO_ROUND,
        timing::STAGE_CBOR_READ,
        cbor_read_start.elapsed(),
        cbor_read_bytes,
    );

    let deserialize_start = std::time::Instant::now();
    let expr = read_cbor(&expr_bytes)?;
    let (table, warnings) = read_metadata(&meta_bytes)?;
    timing::record_stage(
        timing::NO_NODE,
        timing::NO_ROUND,
        timing::STAGE_CBOR_DESERIALIZE,
        deserialize_start.elapsed(),
        0,
    );
    // Runtime unresolved-error naming — see lib.rs twin sites.
    tidepool_codegen::host_fns::register_var_names(&warnings.var_names);
    tidepool_codegen::host_fns::register_poisoned_externals(&warnings.poisoned);

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
        asks: Vec<YieldSite>,
        wrapped_source: String,
    },
    Expr {
        variant: usize,
        asks: Vec<YieldSite>,
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

fn decode_ask(v: &CborValue) -> Result<YieldSite, CompileError> {
    let arr = cbor_expect_array(v, "typed suspension site")?;
    if arr.len() != 7 && arr.len() != 8 {
        return Err(CompileError::ExtractFailed(format!(
            "TurnOut CBOR: expected typed suspension site with 7 or 8 fields, got {}",
            arr.len()
        )));
    }
    let reply_declaration = match arr.get(7) {
        None | Some(CborValue::Null) => None,
        Some(value) => Some(cbor_expect_text(value, "reply declaration")?.to_owned()),
    };
    let site = cbor_as_u64(&arr[0], "Ask site")?;
    let origin = cbor_expect_text(&arr[1], "Ask origin")?.to_string();
    let ordinal = cbor_as_u64(&arr[2], "Ask ordinal")?;
    let answer_type = cbor_expect_text(&arr[3], "Ask answer type")?.to_string();
    let modules = decode_string_array(&arr[4], "Ask modules")?;
    let heads = decode_nominal_heads(&arr[5], "Ask nominal heads")?;
    let inputs = cbor_expect_array(&arr[6], "site input types")?
        .iter()
        .map(|input| {
            let input = cbor_expect_array_len(input, 3, "site input type")?;
            Ok(SiteType {
                ty: cbor_expect_text(&input[0], "site input type name")?.to_string(),
                modules: decode_string_array(&input[1], "site input type modules")?,
                heads: decode_nominal_heads(&input[2], "site input nominal heads")?,
            })
        })
        .collect::<Result<Vec<_>, CompileError>>()?;
    Ok(YieldSite {
        reply_declaration,
        site,
        origin,
        ordinal,
        ty: answer_type,
        modules,
        heads,
        inputs,
    })
}

fn decode_nominal_heads(value: &CborValue, what: &str) -> Result<Vec<NominalHead>, CompileError> {
    cbor_expect_array(value, what)?
        .iter()
        .map(|head| {
            let head = cbor_expect_array_len(head, 3, "nominal type head")?;
            Ok(NominalHead {
                unit: cbor_expect_text(&head[0], "nominal type unit")?.to_string(),
                module: cbor_expect_text(&head[1], "nominal type module")?.to_string(),
                name: cbor_expect_text(&head[2], "nominal type name")?.to_string(),
            })
        })
        .collect()
}

fn decode_asks(v: &CborValue) -> Result<Vec<YieldSite>, CompileError> {
    cbor_expect_array(v, "asks")?
        .iter()
        .map(decode_ask)
        .collect()
}

fn decode_cell_out(bytes: &[u8]) -> Result<CellCheck, CompileError> {
    let value: CborValue = ciborium::de::from_reader(bytes).map_err(|error| {
        CompileError::ExtractFailed(format!("CellOut CBOR: malformed: {error}"))
    })?;
    let root = cbor_expect_array_len(&value, 3, "CellOut")?;
    let items = cbor_expect_array(&root[0], "cell items")?
        .iter()
        .map(decode_cell_item)
        .collect::<Result<Vec<_>, _>>()?;
    let pins = cbor_expect_array(&root[1], "cell pins")?
        .iter()
        .map(decode_checked_binder_pin)
        .collect::<Result<Vec<_>, _>>()?;
    let checked_source = cbor_expect_text(&root[2], "checked cell source")?.to_owned();
    Ok(CellCheck {
        items,
        pins,
        checked_source,
    })
}

fn decode_cell_item(value: &CborValue) -> Result<CellAnalysisItem, CompileError> {
    let fields = cbor_expect_array_len(value, 4, "cell item")?;
    let span = cbor_expect_array_len(&fields[0], 4, "cell item span")?;
    let span = CellSourceSpan {
        start_line: cbor_as_usize(&span[0], "cell span start line")?,
        start_column: cbor_as_usize(&span[1], "cell span start column")?,
        end_line: cbor_as_usize(&span[2], "cell span end line")?,
        end_column: cbor_as_usize(&span[3], "cell span end column")?,
    };
    let kind = match cbor_expect_text(&fields[1], "cell item kind")? {
        "decl" => TurnKind::Decl,
        "bind" => TurnKind::Bind,
        "expr" => TurnKind::Expr,
        other => {
            return Err(CompileError::ExtractFailed(format!(
                "CellOut CBOR: unknown cell item kind {other:?}"
            )))
        }
    };
    let source = cbor_expect_text(&fields[2], "cell item source")?.to_owned();
    let verdict = cbor_expect_array_len(&fields[3], 2, "cell item verdict")?;
    Ok(CellAnalysisItem {
        span,
        source,
        verdict: TurnClassification {
            kind,
            binders: decode_string_array(&verdict[0], "cell item binders")?,
            items: decode_export_items(&verdict[1])?,
        },
    })
}

fn decode_checked_binder_pin(value: &CborValue) -> Result<CheckedBinderPin, CompileError> {
    let fields = cbor_expect_array_len(value, 3, "checked binder pin")?;
    Ok(CheckedBinderPin {
        key: cbor_expect_text(&fields[0], "checked binder pin key")?.to_owned(),
        ty: cbor_expect_text(&fields[1], "checked binder pin type")?.to_owned(),
        heads: decode_nominal_heads(&fields[2], "checked binder pin heads")?,
    })
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

    let mut cmd = extract_cmd()?;
    for path in &paths {
        cmd.input(path);
    }
    cmd.classify().classify_out(&out_path);

    let endpoint = cmd.bind().map_err(map_notfound)?;
    crate::paths::apply_build_products_dir(&mut cmd, &endpoint);
    let run = endpoint.execute(&cmd).map_err(map_notfound)?;
    let output = &run.output;
    // A failed classification still cost a real subprocess spawn — attribute
    // its extract phases the same as a successful one, before the early
    // return below.
    forward_extract_timing(&run.stderr_lossy(), "classify");
    // `classifyBlock` is total over source text: if neither declaration nor
    // statement parsing succeeds it returns `Expr`. A typed worker failure is
    // therefore infrastructure, while a claimed source failure or malformed
    // response means the deployed classifier does not implement this
    // protocol and is reported as version skew.
    if let Err(error) =
        crate::diag::decode_extract_result(run.success(), &output.stdout, &output.stderr)
    {
        return match error {
            CompileError::WorkerFailure(_) => Err(error),
            CompileError::Diagnostics(diags) => {
                let detail = diags
                    .iter()
                    .map(|d| d.message.as_str())
                    .collect::<Vec<_>>()
                    .join("\n\n");
                Err(classify_protocol_failure(detail))
            }
            CompileError::MalformedDiagnostics(detail) => Err(classify_protocol_failure(detail)),
            other => Err(other),
        };
    }

    let json = std::fs::read_to_string(&out_path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            CompileError::MissingOutput(out_path.clone())
        } else {
            CompileError::Io(error)
        }
    })?;
    parse_classify_json(&json, items.len())
}

fn classify_protocol_failure(detail: String) -> CompileError {
    CompileError::MalformedDiagnostics(format!(
        "block classify failed; the deployed tidepool-extract is probably stale \
         (it must support --classify). Redeploy both sides — scripts/redeploy.sh. \
         Extract reported: {detail}"
    ))
}

/// Parse `{"verdicts":[{kind,binders,items}, ...]}`. A verdict count that does not
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

/// Strict wire-shape validation for one classify verdict. This lane has no
/// user-error mode (see `classify_block`'s non-zero-exit handling above): GHC
/// always emits exactly `decl`/`bind`/`expr` for `kind` and a well-formed
/// string array for `binders` (`classifyTurn` rule 6 already folds an
/// unparseable turn into an `expr` VERDICT on the Haskell side — that
/// reclassification is GHC's job, not Rust's). So any shape this function
/// can't recognize is a corrupted or version-skewed wire payload, never a
/// user-Haskell condition — reject it loudly as `MalformedDiagnostics` (the
/// same VersionSkew family the non-zero-exit path above uses) instead of
/// silently defaulting to `expr` or dropping binders. A defaulted verdict
/// would make a corrupted "bind" turn execute as a discard bind or a plain
/// expression instead of surfacing the corruption.
fn parse_one_verdict(v: &serde_json::Value) -> Result<TurnClassification, CompileError> {
    let kind_str = v
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            CompileError::MalformedDiagnostics(format!(
                "classify verdict: missing or non-string `kind` field: {v}"
            ))
        })?;
    let kind = match kind_str {
        "decl" => TurnKind::Decl,
        "bind" => TurnKind::Bind,
        "expr" => TurnKind::Expr,
        other => {
            return Err(CompileError::MalformedDiagnostics(format!(
                "classify verdict: unknown kind {other:?} (expected decl|bind|expr)"
            )))
        }
    };
    let binders_val = v.get("binders").ok_or_else(|| {
        CompileError::MalformedDiagnostics(format!(
            "classify verdict: missing `binders` field for kind {kind_str:?}"
        ))
    })?;
    let binders_arr = binders_val.as_array().ok_or_else(|| {
        CompileError::MalformedDiagnostics(format!(
            "classify verdict: `binders` field is not an array for kind {kind_str:?}: {binders_val}"
        ))
    })?;
    let binders = binders_arr
        .iter()
        .map(|x| {
            x.as_str().map(str::to_string).ok_or_else(|| {
                CompileError::MalformedDiagnostics(format!(
                    "classify verdict: non-string binder entry for kind {kind_str:?}: {x}"
                ))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let items = v
        .get("items")
        .ok_or_else(|| {
            CompileError::MalformedDiagnostics(format!(
                "classify verdict: missing `items` field for kind {kind_str:?}"
            ))
        })?
        .as_array()
        .ok_or_else(|| {
            CompileError::MalformedDiagnostics(format!(
                "classify verdict: `items` field is not an array for kind {kind_str:?}"
            ))
        })?
        .iter()
        .map(parse_classify_export_item)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(TurnClassification {
        kind,
        binders,
        items,
    })
}

fn parse_classify_export_item(v: &serde_json::Value) -> Result<ExportItem, CompileError> {
    let malformed = |detail: &str| {
        CompileError::MalformedDiagnostics(format!(
            "classify verdict: malformed declaration item ({detail}): {v}"
        ))
    };
    let fields = v.as_array().ok_or_else(|| malformed("expected array"))?;
    let tag = fields
        .first()
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| malformed("missing string tag"))?;
    let name = || {
        fields
            .get(1)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| malformed("missing string name"))
    };
    let children = || {
        fields
            .get(2)
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| malformed("missing child-name array"))?
            .iter()
            .map(|child| {
                child
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| malformed("non-string child name"))
            })
            .collect::<Result<Vec<_>, _>>()
    };
    match (tag, fields.len()) {
        ("EValue", 2) => Ok(ExportItem::Value { name: name()? }),
        ("EType", 3) => Ok(ExportItem::Type {
            name: name()?,
            cons: children()?,
        }),
        ("EClass", 3) => Ok(ExportItem::Class {
            name: name()?,
            methods: children()?,
        }),
        _ => Err(malformed("unknown tag or arity")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Force tests that replace `TIDEPOOL_EXTRACT` to exercise that process
    /// boundary even when the surrounding test runner owns a compile daemon.
    /// Each nextest case has its own process, but it still inherits the
    /// runner's daemon socket.
    struct TestEnvGuard {
        key: &'static str,
        old: Option<std::ffi::OsString>,
    }

    impl TestEnvGuard {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let old = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, old }
        }

        fn unset(key: &'static str) -> Self {
            let old = std::env::var_os(key);
            std::env::remove_var(key);
            Self { key, old }
        }
    }

    impl Drop for TestEnvGuard {
        fn drop(&mut self) {
            match &self.old {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn whole_cell_check_harvests_downstream_fixed_local_type() {
        let Some(extract) = std::env::var_os("TIDEPOOL_CELL_TEST_EXTRACT") else {
            return;
        };
        let _daemon = TestEnvGuard::unset("TIDEPOOL_EXTRACT_DAEMON_SOCKET");
        let _extract = TestEnvGuard::set("TIDEPOOL_EXTRACT", extract);
        let root = tempfile::tempdir().unwrap();
        let template = concat!(
            "{-# LANGUAGE NoImplicitPrelude #-}\n",
            "module CellCheck where\n",
            "import Prelude\n",
            "__tidepoolCellExpression :: value -> IO ()\n",
            "__tidepoolCellExpression _ = pure ()\n",
            "{{CELL_DECLS}}\n",
            "__cell = do {\n",
            "{{CELL_BODY}}\n",
            "; pure () }\n",
        );
        let cell = concat!(
            "h <- pure (read \"1\")\n",
            "let value = h + (1 :: Int)\n",
            "value\n",
        );
        let checked = check_cell(CellCheckRequest {
            cell_text: cell,
            template,
            include: &[],
            session_root: root.path(),
            inject_modules: &[],
        })
        .unwrap();
        assert_eq!(checked.items.len(), 3);
        assert_eq!(checked.items[0].verdict.binders, ["h"]);
        let pin = checked
            .pins
            .iter()
            .find(|pin| pin.key == "__tidepool_cell_pin_0_h")
            .unwrap();
        assert_eq!(pin.ty, "Int");

        let bind_template = |imports: &str| TurnTemplate {
            kind: TemplateSelector::Bind,
            source: format!(
                "{{-# LANGUAGE NoImplicitPrelude #-}}\n\
                 module SessionBind where\n\
                 import Prelude\n\
                 {imports}\
                 __result :: IO Int\n\
                 __result = do {{\n\
                 {{{{TURN_STMT}}}}\n\
                 ; pure ({{{{BINDERS}}}}) }}\n"
            ),
        };
        let first_templates = [bind_template("")];
        let first_pins = checked.pins_for_item(0).unwrap();
        let first = run_turn_pinned(
            TurnRequest {
                turn_text: &checked.items[0].source,
                templates: &first_templates,
                include: &[],
                session_root: root.path(),
                inject_modules: &[],
                gen: 1,
                verdict: Some(checked.items[0].verdict.clone()),
                target: None,
            },
            &first_pins,
        )
        .unwrap();
        let TurnResult::Bind { bound, .. } = first else {
            panic!("first staged item was not a bind");
        };
        assert_eq!(bound[0].type_display, "Int");

        let injected = vec![bound[0].module.clone()];
        let second_templates = [bind_template(&format!("import {}\n", injected[0]))];
        let second_pins = checked.pins_for_item(1).unwrap();
        let second = run_turn_pinned(
            TurnRequest {
                turn_text: &checked.items[1].source,
                templates: &second_templates,
                include: &[],
                session_root: root.path(),
                inject_modules: &injected,
                gen: 2,
                verdict: Some(checked.items[1].verdict.clone()),
                target: None,
            },
            &second_pins,
        )
        .unwrap();
        let TurnResult::Bind { bound, .. } = second else {
            panic!("second staged item was not a bind");
        };
        assert_eq!(bound[0].type_display, "Int");
    }

    #[test]
    fn whole_cell_check_harvests_same_cell_nominal_type() {
        let Some(extract) = std::env::var_os("TIDEPOOL_CELL_TEST_EXTRACT") else {
            return;
        };
        let _daemon = TestEnvGuard::unset("TIDEPOOL_EXTRACT_DAEMON_SOCKET");
        let _extract = TestEnvGuard::set("TIDEPOOL_EXTRACT", extract);
        let root = tempfile::tempdir().unwrap();
        let template = concat!(
            "{-# LANGUAGE NoImplicitPrelude #-}\n",
            "module CellCheck where\n",
            "import Prelude\n",
            "__tidepoolCellExpression :: value -> IO ()\n",
            "__tidepoolCellExpression _ = pure ()\n",
            "{{CELL_DECLS}}\n",
            "__cell = do {\n",
            "{{CELL_BODY}}\n",
            "; pure () }\n",
        );
        let cell = concat!(
            "data G = G Int\n",
            "h <- pure Nothing\n",
            "let fixed = h :: Maybe G\n",
            "fixed\n",
        );
        let checked = check_cell(CellCheckRequest {
            cell_text: cell,
            template,
            include: &[],
            session_root: root.path(),
            inject_modules: &[],
        })
        .unwrap();
        let pins = checked.pins_for_item(1).unwrap();
        assert_eq!(pins.len(), 1);
        assert_eq!(pins[0].ty, "Maybe G");
        assert!(pins[0]
            .heads
            .iter()
            .any(|head| head.module == "CellCheck" && head.name == "G"));

        let session = tidepool_repr::SessionId((u64::from(std::process::id()) << 32) | 0x4345_4c4c);
        let prelude = tidepool_testing::eval_harness::prelude_path();
        let mut declarations = crate::session::SessionLib::open(
            session,
            root.path(),
            crate::session::ModuleEnv::standalone_default(),
        )
        .unwrap()
        .with_validation_include(vec![prelude.clone()]);
        let generation = declarations.define("data G = G Int").unwrap();
        let lib_module = format!("Tidepool.Session.Lib.G{}", generation.0);
        let templates = [TurnTemplate {
            kind: TemplateSelector::Bind,
            source: format!(
                "{{-# LANGUAGE NoImplicitPrelude #-}}\n\
                 module SessionNominalBind where\n\
                 import Prelude\n\
                 import {lib_module}\n\
                 __result :: IO (Maybe G)\n\
                 __result = do {{\n\
                 {{{{TURN_STMT}}}}\n\
                 ; pure ({{{{BINDERS}}}}) }}\n"
            ),
        }];
        let staged = run_turn_pinned(
            TurnRequest {
                turn_text: &checked.items[1].source,
                templates: &templates,
                include: &[root.path(), prelude.as_path()],
                session_root: root.path(),
                inject_modules: &[],
                gen: 1,
                verdict: Some(checked.items[1].verdict.clone()),
                target: None,
            },
            &pins,
        )
        .unwrap();
        let TurnResult::Bind { bound, .. } = staged else {
            panic!("same-cell nominal staged item was not a bind");
        };
        assert_eq!(bound[0].type_display, "Maybe G");
    }

    /// [`PREAMBLE_DEFAULT_MARKER`] is duplicated (not depended-on) from
    /// `tidepool_mcp::PREAMBLE_DEFAULT_DECL` to avoid a circular crate
    /// dependency — this pins the two from drifting apart silently.
    #[test]
    fn preamble_default_marker_matches_mcp_constant() {
        assert_eq!(PREAMBLE_DEFAULT_MARKER, tidepool_mcp::PREAMBLE_DEFAULT_DECL);
    }

    #[test]
    fn effectful_expression_is_inferred_under_the_exact_workbench_row() {
        let row = "ActorEffects";
        let effectful = assemble_expression_module(
            "module Expr where\n",
            "__result",
            row,
            "complete action",
            ExpressionLift::Effectful,
        );
        assert!(effectful.contains(&format!(
            "__tidepoolInEffectRow :: Eff {row} value -> Eff {row} value"
        )));
        assert!(effectful.contains("__workbenchValue = __tidepoolInEffectRow $"));
        assert!(!effectful.contains(&format!("Eff {row} _")));

        let pure = assemble_expression_module(
            "module Expr where\n",
            "__result",
            row,
            "42",
            ExpressionLift::Pure,
        );
        assert!(!pure.contains(&format!("Eff {row} _")));
    }

    #[test]
    fn display_expression_keeps_value_and_uses_qualified_private_rendering() {
        let row = "ActorEffects";
        let source = assemble_display_expression_module(
            "module Expr where\n",
            "__result",
            row,
            "effectfulValue",
            ExpressionLift::Effectful,
        );

        assert!(source.contains("__value <- __workbenchValue"));
        assert!(source.contains("pure (__value, T.pack (show __value))"));
        assert!(!source.contains("pure (__value, pack (show __value))"));
        assert_eq!(source.matches("effectfulValue").count(), 1);
        let (start, end) = turn_user_code_line_range(&source, "effectfulValue")
            .expect("the generated module should locate the submitted expression");
        assert_eq!(start, end);
        assert_eq!(source.lines().nth(start - 1), Some("effectfulValue"));

        let pure = assemble_display_expression_module(
            "module Expr where\n",
            "__result",
            row,
            "pureValue",
            ExpressionLift::Pure,
        );
        let opaque = assemble_opaque_expression_module(
            "module Expr where\n",
            "__result",
            row,
            "opaqueValue",
            ExpressionLift::Effectful,
        );
        let pure_opaque = assemble_opaque_expression_module(
            "module Expr where\n",
            "__result",
            row,
            "pureOpaqueValue",
            ExpressionLift::Pure,
        );
        assert!(pure.contains("T.pack (show __workbenchValue)"));
        assert!(opaque.contains("T.pack \"<opaque value>\""));
        assert!(pure_opaque.contains("T.pack \"<opaque value>\""));
        assert!(!pure.contains(" pack "));
        assert!(!opaque.contains(" pack "));
        assert!(!pure_opaque.contains(" pack "));
    }

    #[test]
    fn pure_expression_fallback_rejects_effect_actions_by_type() {
        let source = assemble_opaque_expression_module(
            "module Expr where\ndefault (Int)\n",
            "__result",
            "'[]",
            "badAction",
            ExpressionLift::Pure,
        );

        assert!(source.contains("TidepoolPureWorkbenchValue (Eff effects value)"));
        assert!(source.contains("an Eff action must typecheck in the current workbench effect row"));
        assert!(source.contains("__value = __tidepoolPureWorkbenchValue $ badAction"));
    }

    #[test]
    fn inspection_module_preserves_expression_and_generalizes_probe() {
        let preamble = concat!(
            "{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}\n",
            "module Expr where\n",
            "default (Int)\n",
        );
        let expression = "do\n  x <- pure 1\n  pure x\n";
        let source = assemble_inspection_module(
            preamble,
            "Tidepool.Prelude\nTidepool.Session.Lib.G7",
            &[expression.into()],
        );

        assert!(source.contains("NoImplicitPrelude, NoMonomorphismRestriction,"));
        assert!(source.contains("import Tidepool.Prelude\nimport Tidepool.Session.Lib.G7\n"));
        assert!(source.contains(&format!(
            "__tidepool_inspect_0 = let {{\n __b =\n{expression} }} in __b\n"
        )));
        assert_eq!(source.matches("NoMonomorphismRestriction").count(), 1);
    }

    /// An empty item list must short-circuit before extractor resolution.
    /// The deliberately invalid endpoint makes any accidental boundary
    /// crossing fail this otherwise trivial no-op.
    #[test]
    fn classify_block_empty_items_short_circuits_without_spawning() {
        let _daemon = TestEnvGuard::unset("TIDEPOOL_EXTRACT_DAEMON_SOCKET");
        let _extract = TestEnvGuard::set("TIDEPOOL_EXTRACT", "/nonexistent/tidepool-extract-test");

        let result = classify_block(&[]).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn classify_block_verdict_count_mismatch_is_clean_error() {
        let err = parse_classify_json(
            r#"{"verdicts":[{"kind":"bind","binders":["x"],"items":[]}]}"#,
            2,
        )
        .unwrap_err();
        assert!(
            matches!(err, CompileError::ExtractFailed(_)),
            "expected ExtractFailed, got {err:?}"
        );
    }

    #[test]
    fn parses_bind_classification() {
        let cs = parse_classify_json(
            r#"{"verdicts":[{"kind":"bind","binders":["x"],"items":[]}]}"#,
            1,
        )
        .unwrap();
        assert_eq!(cs[0].kind, TurnKind::Bind);
        assert_eq!(cs[0].binders, vec!["x".to_string()]);
    }

    #[test]
    fn parses_expr_classification() {
        let cs = parse_classify_json(
            r#"{"verdicts":[{"kind":"expr","binders":[],"items":[]}]}"#,
            1,
        )
        .unwrap();
        assert_eq!(cs[0].kind, TurnKind::Expr);
        assert!(cs[0].binders.is_empty());
    }

    #[test]
    fn parses_decl_classification() {
        let cs = parse_classify_json(
            r#"{"verdicts":[{"kind":"decl","binders":["sq"],"items":[["EValue","sq"]]}]}"#,
            1,
        )
        .unwrap();
        assert_eq!(cs[0].kind, TurnKind::Decl);
        assert_eq!(cs[0].binders, vec!["sq".to_string()]);
        assert_eq!(cs[0].items, vec![ExportItem::Value { name: "sq".into() }]);
    }

    /// Any `kind` other than `decl`/`bind`/`expr` is a loud infrastructure
    /// error, in the same `MalformedDiagnostics` (→ VersionSkew) family as
    /// the non-zero-exit path above — this lane has no user-error mode. A
    /// silent default to `TurnKind::Expr` would let a corrupted "bind"
    /// verdict run as a bare expression instead of surfacing the corruption.
    #[test]
    fn unknown_kind_is_malformed_diagnostics_not_silent_expr() {
        let err = parse_classify_json(
            r#"{"verdicts":[{"kind":"weird","binders":[],"items":[]}]}"#,
            1,
        )
        .unwrap_err();
        assert!(
            matches!(err, CompileError::MalformedDiagnostics(_)),
            "expected MalformedDiagnostics, got {err:?}"
        );
    }

    /// A missing `kind` field is a loud infrastructure error, not a silent
    /// `TurnKind::Expr` default.
    #[test]
    fn missing_kind_is_malformed_diagnostics_not_silent_expr() {
        let err =
            parse_classify_json(r#"{"verdicts":[{"binders":["x"],"items":[]}]}"#, 1).unwrap_err();
        assert!(
            matches!(err, CompileError::MalformedDiagnostics(_)),
            "expected MalformedDiagnostics, got {err:?}"
        );
    }

    /// Any non-string element in `binders` rejects the whole verdict — a
    /// silently dropped entry (`["x", 5]` decoding as `["x"]`) would compile
    /// a bind against the wrong binder set instead of failing.
    #[test]
    fn non_string_binder_is_malformed_diagnostics_not_silently_dropped() {
        let err = parse_classify_json(
            r#"{"verdicts":[{"kind":"bind","binders":["x",5],"items":[]}]}"#,
            1,
        )
        .unwrap_err();
        assert!(
            matches!(err, CompileError::MalformedDiagnostics(_)),
            "expected MalformedDiagnostics, got {err:?}"
        );
    }

    /// A malformed or missing `binders` field is a loud infrastructure
    /// error — a silent `vec![]` default would turn a corrupted "bind"
    /// verdict into a discard bind.
    #[test]
    fn malformed_binder_list_is_malformed_diagnostics_not_silent_empty() {
        let non_array = parse_classify_json(
            r#"{"verdicts":[{"kind":"bind","binders":"x","items":[]}]}"#,
            1,
        )
        .unwrap_err();
        assert!(
            matches!(non_array, CompileError::MalformedDiagnostics(_)),
            "expected MalformedDiagnostics for non-array binders, got {non_array:?}"
        );

        let missing = parse_classify_json(r#"{"verdicts":[{"kind":"bind"}]}"#, 1).unwrap_err();
        assert!(
            matches!(missing, CompileError::MalformedDiagnostics(_)),
            "expected MalformedDiagnostics for missing binders, got {missing:?}"
        );
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
        let _extract = TestEnvGuard::set("TIDEPOOL_EXTRACT", "/nonexistent/tidepool-extract-test");
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
                items: Vec::new(),
            }),
            target: None,
        };
        let err = run_turn(req).unwrap_err();
        assert!(
            matches!(err.error, CompileError::ExtractFailed(_)),
            "expected a clean ExtractFailed, got {err:?}"
        );
    }

    /// Ordered templates are a typechecking fallback, not a blanket recovery
    /// loop. A missing external preprocessor raises an infrastructure
    /// exception rather than a GHC `SourceError`; the worker must surface it
    /// immediately instead of silently compiling the valid second template.
    #[test]
    fn run_turn_does_not_retry_template_after_infrastructure_exception() {
        tidepool_testing::eval_harness::require_extract();
        let session_root = TempDir::new().unwrap();
        let templates = vec![
            TurnTemplate {
                kind: TemplateSelector::Expr,
                source: "{-# OPTIONS_GHC -F -pgmF /definitely/missing/tidepool-turn-preprocessor #-}\nmodule Expr where\n__result = {{TURN}}\n".to_string(),
            },
            TurnTemplate {
                kind: TemplateSelector::Expr,
                source: "module Expr where\n__result = {{TURN}}\n".to_string(),
            },
        ];
        let err = run_turn(TurnRequest {
            turn_text: "1 :: Int",
            templates: &templates,
            include: &[],
            session_root: session_root.path(),
            inject_modules: &[],
            gen: 0,
            verdict: Some(TurnClassification {
                kind: TurnKind::Expr,
                binders: Vec::new(),
                items: Vec::new(),
            }),
            target: None,
        })
        .expect_err("an infrastructure exception must not select the valid fallback template");
        assert!(
            matches!(err.error, CompileError::WorkerFailure(_)),
            "worker infrastructure failure must remain distinct from source rejection: {err:?}"
        );
        assert_eq!(
            crate::classify_compile(&err.error).class,
            crate::FailureClass::Infra
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
                    CborValue::Text("M.result".into()),
                    CborValue::Integer(0.into()),
                    CborValue::Text("Text".into()),
                    CborValue::Array(vec![]),
                    CborValue::Array(vec![]),
                    CborValue::Array(vec![]),
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
                assert_eq!(
                    asks,
                    vec![YieldSite {
                        reply_declaration: None,
                        site: 7,
                        origin: "M.result".into(),
                        ordinal: 0,
                        ty: "Text".into(),
                        modules: Vec::new(),
                        heads: Vec::new(),
                        inputs: Vec::new(),
                    }]
                );
                assert!(wrapped_source.contains("result ="));
            }
            other => panic!("expected Bind, got {other:?}"),
        }
    }

    #[test]
    fn yield_site_preserves_captured_reply_declaration_and_reads_legacy_sites() {
        let mut fields = vec![
            CborValue::Integer(7.into()),
            CborValue::Text("M.request".into()),
            CborValue::Integer(0.into()),
            CborValue::Text("Report".into()),
            CborValue::Array(vec![]),
            CborValue::Array(vec![]),
            CborValue::Array(vec![]),
        ];
        assert_eq!(
            decode_ask(&CborValue::Array(fields.clone()))
                .unwrap()
                .reply_declaration,
            None
        );
        fields.push(CborValue::Text("data Report = Report Int".into()));
        assert_eq!(
            decode_ask(&CborValue::Array(fields.clone()))
                .unwrap()
                .reply_declaration
                .as_deref(),
            Some("data Report = Report Int")
        );
        *fields.last_mut().unwrap() = CborValue::Integer(1.into());
        assert!(decode_ask(&CborValue::Array(fields)).is_err());
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
        // 17-extension pragma block before compiling. Dropping the wrapper is a
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
        // Quasiquote regression, decl side (see `quasiquote_bind` below for
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
        // Quasiquote regression: a quasiquote must still classify as a bind
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
                out.push_str("import Tidepool.QQ (fmt, j, patch, uri)\n");
                out.push_str(&preamble[idx..]);
                out
            }
            None => preamble.to_string(),
        }
    }

    #[test]
    fn turn_classification_corpus_old_and_new_path_agree() {
        tidepool_testing::eval_harness::require_extract();

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
        let effects_dirs = effects_dir.include_paths();
        let mut include: Vec<&Path> = effects_dirs
            .iter()
            .map(std::path::PathBuf::as_path)
            .collect();
        include.push(&prelude_dir);

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
            if case.kind == TurnKind::Decl {
                let (_, want_heads) = decl_expectations(case.name);
                assert_eq!(
                    old.items
                        .iter()
                        .map(ExportItem::head_name)
                        .collect::<Vec<_>>(),
                    want_heads,
                    "{}: classify receipt heads mismatch",
                    case.name
                );
            } else {
                assert!(
                    old.items.is_empty(),
                    "{}: non-declaration verdict carried declaration items",
                    case.name
                );
            }

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
                        matches!(err.error, CompileError::Diagnostics(_)),
                        "{}: expected a real GHC diagnostic, got {err:?}",
                        case.name
                    );
                }
                _ => {
                    let result = run_turn(req)
                        .unwrap_or_else(|e| panic!("{}: run_turn failed: {e:?}", case.name));
                    match (case.kind, result) {
                        (TurnKind::Decl, TurnResult::Decl(receipt)) => {
                            let (want_binders, want_heads) = decl_expectations(case.name);
                            assert_eq!(
                                binder_names(&receipt.binders),
                                want_binders,
                                "{}: new-path decl binders mismatch",
                                case.name
                            );
                            assert_eq!(
                                receipt
                                    .items
                                    .iter()
                                    .map(ExportItem::head_name)
                                    .collect::<Vec<_>>(),
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
