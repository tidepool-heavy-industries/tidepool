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

use tidepool_repr::execution_schema::{
    parse_program, DecodeLimits, PreparedProgram, SymbolIdentity,
};
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
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
    /// Original source items represented by this execution item. A declaration
    /// group contains every declaration's ordinal and span.
    pub source_items: Vec<CellAnalysisSourceItem>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellAnalysisSourceItem {
    pub ordinal: usize,
    pub span: CellSourceSpan,
    pub kind: TurnKind,
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

/// Compiler-parsed header syntax, preserved in authored order. It applies to
/// this source only; later cells start from their configured defaults.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourcePrologue {
    pub pragmas: Vec<LocatedPragma>,
    pub imports: Vec<LocatedImport>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PragmaKind {
    Language,
    OptionsGhc,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocatedPragma {
    pub kind: PragmaKind,
    pub span: CellSourceSpan,
    pub source: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocatedImport {
    pub span: CellSourceSpan,
    /// GHC-rendered complete, single-line import declaration.
    pub source: String,
}

/// The declaration owner renders this compiler-normalized source. Authored
/// text is retained separately when needed for receipts and recovery.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeclarationSource {
    pub prologue: SourcePrologue,
    pub body: String,
}

impl SourcePrologue {
    pub fn pragma_text(&self) -> String {
        self.pragmas
            .iter()
            .map(|pragma| format!("{}\n", pragma.source))
            .collect()
    }

    pub fn workbench_imports(&self) -> super::SourceImports {
        super::SourceImports::from_specs(self.imports.iter().map(|import| {
            // The worker returns normalized import declarations, never raw cell lines.
            &import.source["import ".len()..]
        }))
    }

    pub fn import_text(&self) -> String {
        self.imports
            .iter()
            .map(|import| format!("{}\n", import.source))
            .collect()
    }
}

impl DeclarationSource {
    /// Reconstruct replay source from compiler-owned fragments. No syntax is
    /// inferred from authored lines, and compiler option ordering is retained.
    pub fn replay_source(&self, external: &super::SourceImports) -> String {
        format!(
            "{}{}{}{}",
            self.prologue.pragma_text(),
            self.prologue.import_text(),
            external.declaration_prefix(),
            self.body
        )
    }
}

/// Successful whole-cell preflight result.
#[derive(Clone, Debug)]
pub struct CellCheck {
    pub prologue: SourcePrologue,
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

/// Checking failure with the GHC source plan, when lexing/classification succeeded.
/// A failed check never makes the plan executable: it carries no trusted pins.
#[derive(Debug, thiserror::Error)]
#[error("{error}")]
pub struct CellCheckFailure {
    pub error: CompileError,
    pub items: Option<Vec<CellAnalysisItem>>,
}

impl From<CompileError> for CellCheckFailure {
    fn from(error: CompileError) -> Self {
        Self { error, items: None }
    }
}

impl From<std::io::Error> for CellCheckFailure {
    fn from(error: std::io::Error) -> Self {
        CompileError::from(error).into()
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
    /// Ask the same compile to also produce this turn's prepared-STG program.
    /// `None` is the Core-only turn every caller performs today.
    pub prepared: Option<PreparedTurn<'a>>,
}

/// The prepared half of a turn request: the caller's live bindings, declared
/// as executable imports so the projection links against them instead of
/// recompiling their bodies. Retaining a binding is only meaningful when a
/// prepared program is requested, so the two travel together.
pub struct PreparedTurn<'a> {
    pub retained: &'a [(SymbolIdentity, u64)],
}

impl PreparedTurn<'static> {
    /// The prepared half of a session's first turn on the current route:
    /// `None` on Core, and on the prepared route a request linked against
    /// nothing (there is no retained binding yet).
    #[must_use]
    pub fn first_turn() -> Option<PreparedTurn<'static>> {
        (super::persistent::EngineKind::from_env() == super::persistent::EngineKind::Prepared)
            .then_some(PreparedTurn { retained: &[] })
    }
}

/// `tidepool-extract-cmd` is a dependency leaf and cannot name
/// `tidepool_repr`'s identity type; this is the one conversion site.
fn extract_identity(identity: &SymbolIdentity) -> tidepool_extract_cmd::SymbolIdentity {
    tidepool_extract_cmd::SymbolIdentity {
        unit: identity.unit.clone(),
        module: identity.module.clone(),
        namespace: identity.namespace.clone(),
        occurrence: identity.occurrence.clone(),
        record_parent: identity.record_parent.clone(),
    }
}

/// The binder a prepared turn PROJECTS: the turn's `__result` effect
/// computation settled to one constructor layer by
/// `Tidepool.Internal.Resume.settle`, so the host reads completion or
/// suspension without walking freer data. Every template assembled here
/// defines it (`Tidepool.Session.preparedScaffoldTargetName` on the worker
/// side); only a prepared turn writes `__prepared.prepared.cbor`.
pub const PREPARED_SCAFFOLD_TARGET: &str = "__prepared";

/// The resume entry every prepared turn admits beside
/// [`PREPARED_SCAFFOLD_TARGET`] (`Tidepool.Session.preparedResumeTargetName`
/// on the worker side): `__resume q x = settle (resumeLifted q x)` re-enters
/// a parked continuation with a lifted answer and settles the result through
/// the same layer the initial run did. The extractor projects it as an
/// auxiliary root; the session refuses to park a suspension of a program
/// that lacks it.
pub const PREPARED_RESUME_TARGET: &str = "__resume";

/// The decode entry every prepared turn admits beside
/// [`PREPARED_SCAFFOLD_TARGET`] and [`PREPARED_RESUME_TARGET`]
/// (`Tidepool.Session.preparedDecodeTargetName` on the worker side):
/// `__decodeValue :: Text -> Either Text Value`, the leaf adapter the
/// session enters to turn a JSON-rendered bridge answer into a retained
/// `Tidepool.Aeson.Value.Value` before splicing it into an outer answer
/// ([`crate::session::prepared`]'s `Value`-carrying-reply lowering). The
/// extractor projects it as an auxiliary root beside
/// [`PREPARED_RESUME_TARGET`].
pub const PREPARED_DECODE_TARGET: &str = "__decodeValue";

/// The generic apply entry every prepared turn admits beside
/// [`PREPARED_SCAFFOLD_TARGET`], [`PREPARED_RESUME_TARGET`], and
/// [`PREPARED_DECODE_TARGET`] (`Tidepool.Session.preparedApplyEntryTargetName`
/// on the worker side): `__applyEntry f n = settle (f (I# n))`, the entry
/// `ResidentSession::run_rooted_entry`/`run_rooted_entry_borrowed`
/// (`tidepool-runtime/src/session/resident.rs`) enters to apply a rooted
/// `Int -> M a` closure to a bare unboxed argument without a Core fragment to
/// compile it into — actor program start, shutdown hooks, actor source, and
/// green-thread bodies all cross this entry on the prepared route. Because
/// `settle` is polymorphic in the settled computation's effect row and
/// result, this entry (unlike the settled scaffold line) is NOT tied to one
/// turn's own type and its result carries a free type variable; the extractor
/// excludes it from auxiliary-root evidence interning for exactly that reason
/// (`Tidepool.ExecutionProjection.lowerAuxiliaryRootEvidence`) while still
/// projecting it as an auxiliary root so the runtime can look it up by name.
pub const PREPARED_APPLY_ENTRY_TARGET: &str = "__applyEntry";

/// The generic apply entry every prepared turn admits beside
/// [`PREPARED_APPLY_ENTRY_TARGET`] (`Tidepool.Session.preparedApplyValueTargetName`
/// on the worker side): `__applyValue f x = settle (f x)`, the entry
/// `ResidentSession::run_rooted_application` enters to apply one rooted
/// Haskell value to another — both retained, neither bridged — without a
/// Core fragment to compile it into (actor mailboxes, where the handler and
/// its protocol-indexed request are both live Haskell values). Same
/// polymorphism and evidence-interning exclusion as
/// [`PREPARED_APPLY_ENTRY_TARGET`].
pub const PREPARED_APPLY_VALUE_TARGET: &str = "__applyValue";

/// The qualified alias every template imports `Tidepool.Internal.Resume` under.
const RESUME_ALIAS: &str = "TidepoolResume";

/// The qualified aliases the decode entry's signature is written under.
/// The scaffold cannot assume a template's own preamble brings `Text` or
/// `Value` into scope (the harness-ctx template imports almost nothing), so
/// it imports both modules itself under aliases no authored code uses.
const TEXT_ALIAS: &str = "TidepoolScaffoldText";
const AESON_VALUE_ALIAS: &str = "TidepoolScaffoldAeson";

/// The qualified alias every template imports `GHC.Exts` under, so
/// [`PREPARED_APPLY_ENTRY_TARGET`] can box its unboxed `Int#` argument
/// through `I#` without depending on a template's own imports.
const SCAFFOLD_EXTS_ALIAS: &str = "TidepoolScaffoldExts";

/// The three lines every executable template ends with: the settled
/// scaffold the prepared route projects, the resume entry it re-enters
/// parked continuations through, and the decode entry it lowers
/// `Value`-carrying answers through. All three are unreachable from
/// `__result`, so the Core closure never sees them. Built from
/// [`prepared_scaffold_binding_named`] (the settled line) and
/// [`prepared_resume_decode_binding`] (the shared resume/decode/apply group)
/// at the fixed [`PREPARED_SCAFFOLD_TARGET`]/[`PREPARED_RESUME_TARGET`]/
/// [`PREPARED_DECODE_TARGET`]/[`PREPARED_APPLY_ENTRY_TARGET`]/
/// [`PREPARED_APPLY_VALUE_TARGET`] names every resident-turn template uses.
///
/// Public so a caller assembling its OWN template outside
/// [`assemble_bind_module`]/[`assemble_expression_module`] (a hand-rolled
/// fixture in a test, say) can still append the exact scaffold a prepared
/// compile requires, rather than hand-duplicating these binder names.
pub fn prepared_scaffold_binding(target: &str) -> String {
    let mut out = prepared_scaffold_binding_named(PREPARED_SCAFFOLD_TARGET, target);
    out.push_str(&prepared_resume_decode_binding());
    out
}

/// As [`prepared_scaffold_binding`]'s settled line, but with the settled
/// binder name supplied by the caller rather than fixed to
/// [`PREPARED_SCAFFOLD_TARGET`]. Every OTHER caller in this module settles
/// exactly one target per compiled module and can use the fixed name via
/// [`prepared_scaffold_binding`]; a caller settling MORE THAN ONE target in
/// the same module (compiling several `--targets` in one spawn against a
/// shared `meta.cbor`) must give each target's settled binding its own name
/// here or the settled bindings collide as duplicate top-level
/// declarations. This is the ONE place the settled line's text is built —
/// [`prepared_scaffold_binding`] is a thin specialization, not a second
/// copy.
///
/// Does NOT emit `__resume`/`__decodeValue` — see
/// [`prepared_resume_decode_binding`]'s doc for why those stay
/// fixed-named and module-shared rather than following `scaffold_target`.
#[must_use]
pub fn prepared_scaffold_binding_named(scaffold_target: &str, target: &str) -> String {
    format!("{scaffold_target} = {RESUME_ALIAS}.settle {target}\n")
}

/// The resume, decode, and generic-apply entries a module needs beside its
/// settled scaffold line(s) — see [`prepared_scaffold_binding`]'s doc for
/// what each does. UNLIKE the settled line itself, these are fixed at
/// [`PREPARED_RESUME_TARGET`]/[`PREPARED_DECODE_TARGET`]/
/// [`PREPARED_APPLY_ENTRY_TARGET`]/[`PREPARED_APPLY_VALUE_TARGET`] no matter
/// how many targets a module settles: the runtime resolves a program's
/// resume/decode/apply roots by looking these exact names up in the
/// program's own top-level bindings (`ProgramFacts::of` in
/// `tidepool-runtime/src/session/prepared.rs` scans for
/// `identity.occurrence == PREPARED_RESUME_TARGET`/`PREPARED_DECODE_TARGET`/
/// `PREPARED_APPLY_ENTRY_TARGET`/`PREPARED_APPLY_VALUE_TARGET`), and none of
/// these four bodies take a target-specific argument (`resumeLifted`/
/// `eitherDecodeValue`/the apply roots' own `settle` are the same computation
/// regardless of which settled entry suspended) — so a module settling
/// several targets ([`with_settled_scaffolds`] in `tidepool-harness::engine`)
/// emits this ONCE for the whole module, not once per target the way
/// [`prepared_scaffold_binding_named`]'s settled line must be.
///
/// Every binding names its parameters on purpose: a point-free
/// `__decodeValue = eitherDecodeValue` compiles to an arity-0 value, and the
/// runtime enters the root with one managed argument, which the entry then
/// refuses ("expected 0 physical scalar slots"). Eta-expanded, the root is a
/// one-argument function with the signature the runtime enters (see `git show
/// bd69b719d`). `__applyEntry`/`__applyValue` take no explicit signature:
/// each has explicit parameters, so the monomorphism restriction never
/// applies and GHC infers `n`'s type as `Int#` directly from its use as
/// `I#`'s argument.
#[must_use]
pub fn prepared_resume_decode_binding() -> String {
    format!(
        "{PREPARED_RESUME_TARGET} q x = {RESUME_ALIAS}.settle ({RESUME_ALIAS}.resumeLifted q x)\n\
         {PREPARED_DECODE_TARGET} :: {TEXT_ALIAS}.Text -> Either {TEXT_ALIAS}.Text {AESON_VALUE_ALIAS}.Value\n\
         {PREPARED_DECODE_TARGET} t = {AESON_VALUE_ALIAS}.eitherDecodeValue t\n\
         {PREPARED_APPLY_ENTRY_TARGET} f n = {RESUME_ALIAS}.settle (f ({SCAFFOLD_EXTS_ALIAS}.I# n))\n\
         {PREPARED_APPLY_VALUE_TARGET} f x = {RESUME_ALIAS}.settle (f x)\n"
    )
}

/// The bare import targets (no leading `import `, one per line)
/// [`prepared_scaffold_binding`]/[`prepared_resume_decode_binding`]'s aliases
/// need in scope. [`with_resume_import`] splices these in through
/// [`insert_preamble_imports`]'s own marker
/// ([`PREAMBLE_DEFAULT_MARKER`], the production preamble's shape); a caller
/// assembling a differently-shaped preamble (a test fixture with its own
/// import-splicing marker) can still get the exact same alias names by
/// splicing this text in itself, rather than hand-duplicating the aliases.
#[must_use]
pub fn resume_import_targets() -> String {
    format!(
        "qualified Tidepool.Internal.Resume as {RESUME_ALIAS}\n\
         qualified Data.Text as {TEXT_ALIAS}\n\
         qualified Tidepool.Aeson.Value as {AESON_VALUE_ALIAS}\n\
         qualified GHC.Exts as {SCAFFOLD_EXTS_ALIAS}"
    )
}

/// The preamble with the settle module (and `GHC.Exts`, and the `MagicHash`
/// extension the apply roots' `I#` reference needs) in scope for
/// [`prepared_scaffold_binding`]/[`prepared_scaffold_binding_named`].
#[must_use]
pub fn with_resume_import(preamble_with_imports: &str) -> String {
    let preamble_with_imports = with_magic_hash(preamble_with_imports);
    insert_preamble_imports(&preamble_with_imports, &resume_import_targets())
}

/// Prepend `{-# LANGUAGE MagicHash #-}` ahead of `preamble`'s own pragma
/// block, unless it is already enabled: parsing `GHC.Exts.I#`/`Int#` needs
/// the extension regardless of what a caller's own template pragma set
/// enables, and a duplicate `LANGUAGE MagicHash` pragma is otherwise harmless
/// but needless. A fresh pragma line ahead of the existing block stays valid
/// Haskell — GHC accepts any number of `LANGUAGE` pragmas before the module
/// header — so this never depends on the shape of the caller's own pragma
/// line.
fn with_magic_hash(preamble: &str) -> String {
    if preamble.contains("MagicHash") {
        return preamble.to_string();
    }
    format!("{{-# LANGUAGE MagicHash #-}}\n{preamble}")
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

/// The one sentence a model gets when a submitted binding's type never became
/// concrete. It names the repair (a signature) rather than the compiler
/// artifact the ambiguity leaked as.
pub const AMBIGUOUS_TYPE_ADVICE: &str =
    "this declaration's type is ambiguous; give it a signature naming the type you \
     meant (the diagnostic above says which constraint was left open)";

/// The advice for an ambiguity GHC blames on a literal rather than on a
/// binding: no signature on a declaration fixes it, only an annotation at the
/// literal itself.
pub const LITERAL_ANNOTATION_ADVICE: &str =
    "this literal's type is ambiguous; annotate the literal: `(\"src/app.rs\" :: Text)`";

/// The advice for [`is_cell_pure_dispatch_ambiguity`]'s shape: a signature
/// repairs nothing here, because nothing the reader wrote is actually
/// polymorphic — `pure`/`return` on a cell's final unit is the ordinary,
/// correct habit inside a `do` block, and [`check_cell_preferring_effectful`]
/// already runs that unit as a workbench action for every cell this
/// diagnostic alone describes. A reader sees this only alongside a genuine,
/// separate error on the same statement — the pinned retry failed too — so
/// the advice still needs to name the real repair rather than send them
/// chasing a signature that fixes nothing.
pub const CELL_PURE_DISPATCH_ADVICE: &str =
    "this cell's last statement is a plain value wrapped in `pure`/`return`, not an action \
     — drop the `pure`/`return` and write the action directly, the way it already works as \
     the last line of an ordinary `do` block";

/// Whether `message` is GHC's diagnostic for the resident workbench's
/// whole-cell preflight (see
/// [`super::workbench::resident_cell_check_template`]) failing to decide
/// whether a cell's final expression is a plain value or a workbench action:
/// an unsolved `Applicative`/`Monad` constraint left open by `pure`/`return`
/// on a type the `TidepoolCellExpression`/`TidepoolCellPure` overlap could
/// not pin down first.
///
/// Deliberately narrow — narrower than the general "Ambiguous type variable"
/// family [`ambiguous_type_advice`] otherwise handles — because
/// [`check_cell_preferring_effectful`] spends a whole extra compile on a
/// positive answer. An ordinary ambiguous binding (an unconstrained `Render
/// a0`, say) does not carry an `Applicative`/`Monad` constraint and does not
/// match.
#[must_use]
fn is_cell_pure_dispatch_ambiguity(message: &str) -> bool {
    message.contains("Ambiguous type variable")
        && message.contains("prevents the constraint")
        && (message.contains("(Applicative ") || message.contains("(Monad "))
}

/// The advice for a cell item that carries a signature whose equation was
/// submitted as a separate item. Each item compiles as its own
/// `module SessionDecls where`, so the signature installs nothing and the
/// equation's item then reports the name as out of scope.
pub const SPLIT_SIGNATURE_ADVICE: &str =
    "a signature and its equation belong in the same cell item";

/// Recognize a GHC diagnostic that only ever means "this type never became
/// concrete", so the model is told to add a signature instead of being handed
/// a compiler-internal name or an instance-resolution trace.
///
/// Defaulting already runs on every generated module — the workbench preamble
/// carries `ExtendedDefaultRules` ([`super::EVAL_PRAGMAS`]) and a
/// `default (Int, Double, Text)` declaration, and both survive into the
/// whole-cell check template. Neither shape below is reachable by defaulting:
///
/// * `GHC.Types.ZonkAny` is what GHC zonks an ungeneralized metavariable to.
///   It reaches source when a checked binder's post-zonk type is replanted
///   into the staged wrapper ([`run_turn_pinned`]), and the wrapper then
///   fails with `Not in scope: type constructor or class GHC.Types.ZonkAny`.
///   The type is already gone by then; no default list can name it.
/// * An overlap that GHC itself reports as depending on the instantiation of
///   a unification variable. Defaulting under `ExtendedDefaultRules` still
///   requires the ambiguous variable's constraint set to carry one of GHC's
///   own standard classes (numeric, `Show`, `Eq`, `Ord`); a solitary `Render`
///   or `TidepoolCellExpression` constraint never qualifies, and a
///   higher-kinded variable (`Render (f0 Double)`) cannot be named by a
///   `default (...)` list at all, which lists only `*`-kinded types.
///
/// GHC's own `Ambiguous type variable … arising from` family is recognized as
/// well, including the effect-row form whose constraint is a `FindElem`. There
/// the repair is the same signature, so the advice names the binding GHC
/// printed and says where its signature goes for the form the submitted text
/// used. An ambiguity GHC blames on a literal takes an annotation at the
/// literal instead, and a signature submitted without its equation takes
/// neither.
///
/// `submitted` is the text the model wrote — the turn or cell item, not the
/// generated wrapper — and decides only between the `let` and top-level
/// signature placements.
#[must_use]
pub fn ambiguous_type_advice(message: &str, submitted: &str) -> Option<String> {
    if message.contains("lacks an accompanying binding") {
        let name = quoted_name_after(message, "The type signature for").unwrap_or("this binding");
        return Some(format!(
            "`{name}` has a signature but no equation in this cell item; {SPLIT_SIGNATURE_ADVICE}"
        ));
    }
    if let Some(advice) = redeclared_type_advice(message) {
        return Some(advice);
    }
    if message.contains("Ambiguous type variable") {
        if message.contains("arising from the literal") || message.contains("IsString") {
            return Some(LITERAL_ANNOTATION_ADVICE.to_owned());
        }
        if is_cell_pure_dispatch_ambiguity(message) {
            return Some(CELL_PURE_DISPATCH_ADVICE.to_owned());
        }
        let Some(name) = quoted_name_after(message, "In an equation for")
            .or_else(|| quoted_name_after(message, "In a pattern binding for"))
        else {
            return Some(AMBIGUOUS_TYPE_ADVICE.to_owned());
        };
        return Some(if let_bound(submitted, name) {
            format!(
                "`{name}`'s type is ambiguous; give it a signature in the same `let` binding: \
                 `let {name} :: T -> U; {name} x = …` (a signature on its own `let` line loses \
                 the argument scope)"
            )
        } else {
            format!(
                "`{name}`'s type is ambiguous; add its signature line directly above the \
                 equation in the same cell item: `{name} :: T -> U`"
            )
        });
    }
    // GHC emits this hint exactly when the overlapping-instance choice hangs
    // on a variable it has not instantiated — the ground-head overlaps a real
    // instance conflict produces carry no such line.
    let unresolved_overlap = message.contains("Overlapping instances for")
        && message.contains("The choice depends on the instantiation of");
    (unresolved_overlap || message.contains("ZonkAny"))
        .then(|| AMBIGUOUS_TYPE_ADVICE.to_owned())
}

/// GHC's `MonadFail` desugaring for a refutable bind in a `do` block, as it
/// reaches a cell: an exception whose text points at the generated wrapper.
const DO_BLOCK_PATTERN_FAILURE: &str = "Pattern match failure in 'do' block at ";

/// Turn a runtime failure that is really a user-level mistake into a cell-level
/// message about the cell.
///
/// A refutable bind — `Right handle <- createWorktree …` — desugars to
/// `fail`, which raises. The concise form is the right thing to write in a
/// throwaway cell, so this does not discourage it; what reaches the reader is
/// the problem. Today that is "prepared execution failed: Haskell exception
/// raised: Pattern match failure in 'do' block at /tmp/…/Expr.hs:78:1-10":
/// three layers of engine detail and a location inside a generated wrapper the
/// reader never wrote, and no mention of which bind or what it was given. The
/// bind is recovered from the submitted cell rather than from the wrapper's
/// coordinates, and the reader is told how to see the value if they want it.
#[must_use]
pub fn runtime_failure_advice(message: &str, cell_text: &str) -> Option<String> {
    if !message.contains(DO_BLOCK_PATTERN_FAILURE) {
        return None;
    }
    let binds = refutable_binds(cell_text);
    let Some((line, pattern, bound)) = binds.first() else {
        return Some(
            "a pattern bind did not match, so the cell stopped there; \
             bind that value plainly to see what it was"
                .to_owned(),
        );
    };
    let name = bound.as_deref().unwrap_or("result");
    let where_ = if binds.len() == 1 {
        format!("`{pattern}` on line {line}")
    } else {
        format!("a pattern bind, first `{pattern}` on line {line},")
    };
    Some(format!(
        "{where_} did not match, so the cell stopped there; the value is not \
         shown, so bind it plainly (`{name} <- …`) to see what it was"
    ))
}

/// Lines of `cell_text` that bind through a refutable constructor pattern,
/// as (1-based line, the pattern as written, the last name it binds).
fn refutable_binds(cell_text: &str) -> Vec<(usize, String, Option<String>)> {
    cell_text
        .lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let (left, _) = line.split_once("<-")?;
            let pattern = left.trim();
            let mut words = pattern.split_whitespace();
            // A constructor pattern starts with an upper-case name; a plain
            // binder or a tuple does not, and neither can fail to match.
            let head = words.next()?;
            if !head.starts_with(char::is_uppercase) {
                return None;
            }
            let bound = words
                .last()
                .filter(|word| word.starts_with(char::is_lowercase))
                .map(str::to_owned);
            Some((index + 1, pattern.to_owned(), bound))
        })
        .collect()
}

/// Recognize GHC's `Ambiguous occurrence` diagnostic when the competing
/// candidates come from two different cell generations
/// (`Tidepool.Session.Lib.G<n>`). Each cell's declarations compile into their
/// own `Tidepool.Session.Lib.G<n>` module, so re-running a declaration a
/// prior cell already installed leaves it live in two generations at once —
/// GHC reports this as an ordinary ambiguous-occurrence error, which names a
/// scope problem the model created, not a type it needs to add. A signature
/// repairs nothing here, so this is a separate advice family from
/// [`ambiguous_type_advice`]'s ambiguous-type shapes, called from it as one
/// more recognized diagnostic.
///
/// The two candidate shapes need DIFFERENT advice, so this distinguishes
/// them from GHC's own wording rather than treating every hit as a type:
/// - "the field `f' of record `M.T'" (or "the method … of class `M.C'") is a
///   genuine TYPE/class re-declaration — the harder case: values already
///   bound in the session's value plane were built against the OLD shape, so
///   re-declaring it stays refused (see the module doc on `redeclared_type_advice`).
/// - a bare `M.name` occurrence (no "of record"/"of class" framing) is a
///   plain VALUE re-declared across generations. A value redeclaration is
///   meant to shadow silently with no error at all (GHCi parity — see
///   `render_module`'s `hidden_prior`); seeing GHC still report one here
///   means that shadowing didn't apply for this turn, which is a
///   session-scoping gap, not a type the reader introduced. The message must
///   say so plainly instead of telling them never to re-declare "a type".
fn redeclared_type_advice(message: &str) -> Option<String> {
    if !message.contains("Ambiguous occurrence") {
        return None;
    }
    let occurrence = quoted_name_after(message, "Ambiguous occurrence")?;
    let mut generations: Vec<(&str, &str, bool)> = Vec::new();
    for (position, _) in message.match_indices("Tidepool.Session.Lib.G") {
        let rest = &message[position..];
        let after_prefix = &rest["Tidepool.Session.Lib.G".len()..];
        let digits_len = after_prefix
            .chars()
            .take_while(char::is_ascii_digit)
            .count();
        if digits_len == 0 {
            continue;
        }
        let module = &rest[.."Tidepool.Session.Lib.G".len() + digits_len];
        let Some(after_module) = after_prefix[digits_len..].strip_prefix('.') else {
            continue;
        };
        let name = after_module
            .split(|character: char| !(character.is_alphanumeric() || character == '_'))
            .next()
            .unwrap_or("");
        if name.is_empty() {
            continue;
        }
        let is_type_member = names_a_type_member(&message[..position]);
        if !generations
            .iter()
            .any(|(m, n, _)| *m == module && *n == name)
        {
            generations.push((module, name, is_type_member));
        }
    }
    let name = generations.first()?.1;
    let matches: Vec<&(&str, &str, bool)> = generations
        .iter()
        .filter(|(_, candidate, _)| *candidate == name)
        .collect();
    if matches.len() < 2 {
        return None;
    }
    let modules = matches
        .iter()
        .map(|(module, _, _)| *module)
        .collect::<Vec<_>>()
        .join(" and ");
    let is_type_redeclaration = matches.iter().any(|(_, _, is_type_member)| *is_type_member);
    Some(if is_type_redeclaration {
        format!(
            "`{occurrence}` is ambiguous because `{name}` was re-declared in this session \
             ({modules} both define it); values already in the session were built with the \
             earlier `{name}`, so re-declaring a type is refused here — reuse the earlier \
             `{name}` declaration instead of re-running it, or rename this one and its fields \
             if you meant a distinct type"
        )
    } else {
        format!(
            "`{occurrence}` is ambiguous because `{name}` was declared again in this session \
             ({modules} both define it); an ordinary declaration is meant to shadow the earlier \
             one automatically, so this is a session-scoping gap, not something wrong with your \
             code — reuse `{name}` as already declared, or give this one a different name to \
             work around it for now"
        )
    })
}

/// Whether the `Tidepool.Session.Lib.G<n>.<name>` occurrence ending right
/// before `before` names a record field or class method — GHC's own words
/// for "this identifies an actual TYPE", which a bare qualified value
/// occurrence never carries.
fn names_a_type_member(before: &str) -> bool {
    [
        "of record `",
        "of record \u{2018}",
        "of class `",
        "of class \u{2018}",
    ]
    .iter()
    .any(|marker| before.ends_with(marker))
}

/// Plain-VALUE names ambiguous between exactly `previous_module` and
/// `candidate_module` in a GHC `Ambiguous occurrence` diagnostic.
///
/// This is the shape [`redeclared_type_advice`] alone cannot repair: a cell
/// that both RE-DECLARES a name and USES it from a bind statement in the
/// SAME cell. The whole-cell preflight check (`resident_workbench::
/// prepare_cell`) compiles the cell's own fresh declarations directly into a
/// module already named for the NEXT generation (`candidate_module`),
/// alongside an unqualified import of the CURRENT generation
/// (`previous_module`) that was built before this cell's own redeclarations
/// were known — so both are visible at once and GHC reports the ambiguity
/// this function recognizes. The caller retries with `previous_module
/// hiding (...)` naming exactly what this returns — the same shadowing
/// every other generation boundary already gets via `render_module`'s
/// `hidden_prior`.
///
/// Excludes any occurrence GHC frames as "the field ... of record"/"the
/// method ... of class" — those name a genuine type/class collision, which
/// must stay refused rather than silently hidden (see
/// [`redeclared_type_advice`]).
#[must_use]
pub fn same_cell_value_collisions(
    message: &str,
    previous_module: &str,
    candidate_module: &str,
) -> Vec<String> {
    if !message.contains("Ambiguous occurrence") || previous_module == candidate_module {
        return Vec::new();
    }
    let names_in = |module: &str| -> Vec<&str> {
        let prefix = format!("{module}.");
        let mut names: Vec<&str> = Vec::new();
        for (position, _) in message.match_indices(prefix.as_str()) {
            if names_a_type_member(&message[..position]) {
                continue;
            }
            let after = &message[position + prefix.len()..];
            let name = after
                .split(|character: char| !(character.is_alphanumeric() || character == '_'))
                .next()
                .unwrap_or("");
            if !name.is_empty() && !names.contains(&name) {
                names.push(name);
            }
        }
        names
    };
    let previous_names = names_in(previous_module);
    let candidate_names = names_in(candidate_module);
    previous_names
        .into_iter()
        .filter(|name| candidate_names.contains(name))
        .map(str::to_owned)
        .collect()
}

/// The identifier GHC prints immediately after `prefix`, quoted as `‘name’` or,
/// under ASCII diagnostics, as `` `name' ``.
fn quoted_name_after<'a>(message: &'a str, prefix: &str) -> Option<&'a str> {
    let rest = message.split_once(prefix)?.1.trim_start();
    let (open, close) = if rest.starts_with('\u{2018}') {
        ('\u{2018}', '\u{2019}')
    } else {
        ('`', '\'')
    };
    let name = rest.strip_prefix(open)?.split(close).next()?;
    (!name.is_empty() && !name.contains(char::is_whitespace)).then_some(name)
}

/// Whether `name` is bound inside a `let` in the submitted text. A `let`
/// binding needs `let name :: T -> U; name x = …` on one binding: splitting the
/// signature onto its own `let` line loses the argument scope.
fn let_bound(submitted: &str, name: &str) -> bool {
    let mut open_let: Option<usize> = None;
    for line in submitted.lines() {
        let body = line.trim_start();
        let indent = line.len() - body.len();
        if open_let.is_some_and(|column| body.is_empty() || indent <= column) {
            open_let = None;
        }
        if open_let.is_some() && starts_with_token(body, name) {
            return true;
        }
        if let Some(position) = find_let_token(line) {
            if starts_with_token(line[position + 3..].trim_start(), name) {
                return true;
            }
            open_let = Some(position);
        }
    }
    false
}

/// The column of the first `let` keyword in `line`, ignoring `let` inside a
/// longer word.
fn find_let_token(line: &str) -> Option<usize> {
    line.match_indices("let")
        .find(|(position, _)| {
            let before = line[..*position].chars().next_back();
            let after = line[position + 3..].chars().next();
            before.is_none_or(|character| !is_name_character(character))
                && after.is_none_or(char::is_whitespace)
        })
        .map(|(position, _)| position)
}

/// Whether `text` opens with `name` as a whole identifier.
fn starts_with_token(text: &str, name: &str) -> bool {
    text.strip_prefix(name)
        .is_some_and(|rest| !rest.starts_with(is_name_character))
}

fn is_name_character(character: char) -> bool {
    character.is_alphanumeric() || character == '_' || character == '\''
}

/// A compile rejection as its reader gets it: the rendered text exactly as
/// before, and the diagnostics that produced it kept as data beside it.
///
/// `output` is authoritative for display and is never derived from
/// `diagnostics`. `diagnostics` is GHC's own report as
/// [`crate::diag::render_diagnostics_structured`] resolved it — the same
/// coordinates the text shows. The two can differ in one direction only: the
/// advice layer below may REPLACE GHC's text with a plain-language
/// explanation when the diagnostic is an artifact of how a turn is wrapped
/// (see [`Diagnostic::Artifact`]), and in that case `diagnostics` still
/// carries the underlying GHC report the text dropped. It is never the other
/// way round — `diagnostics` never says less than `output`.
///
/// A rejection that is not a GHC diagnostics report at all (a missing
/// artifact, a toolchain skew) renders its classified message with an empty
/// `diagnostics`: there is no diagnostic to structure, and inventing one
/// would be worse than saying so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileRejection {
    pub output: String,
    pub diagnostics: Vec<crate::diag::StructuredDiagnostic>,
}

impl CompileRejection {
    /// A rejection with nothing structured behind it.
    fn unstructured(output: String) -> Self {
        Self {
            output,
            diagnostics: Vec::new(),
        }
    }
}

impl From<String> for CompileRejection {
    fn from(output: String) -> Self {
        Self::unstructured(output)
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
    render_turn_compile_rejection(error, attempted_source, turn_text, label).output
}

/// [`render_turn_compile_error`]'s text plus its diagnostics as data.
#[must_use]
pub fn render_turn_compile_rejection(
    error: &CompileError,
    attempted_source: Option<&str>,
    turn_text: &str,
    label: &str,
) -> CompileRejection {
    let CompileError::Diagnostics(diagnostics) = error else {
        return CompileRejection::unstructured(crate::classify_compile(error).message);
    };
    let Some(source) = attempted_source else {
        return CompileRejection::unstructured(crate::classify_compile(error).message);
    };
    let anchor = extract_module_name(source)
        .map(|module| format!("{}.hs", module.replace('.', "/")))
        .unwrap_or_else(|| "Expr.hs".into());
    let (line_offset, col_indent) = turn_user_code_offset(source).unwrap_or((0, 0));
    let user_lines = turn_user_code_line_range(source, turn_text);
    let rendered = crate::diag::render_diagnostics_structured(
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
    );
    CompileRejection {
        output: with_advice(rendered.text, |text| advice_for(text, turn_text)),
        diagnostics: rendered.diagnostics,
    }
}

/// Render whole-cell diagnostics against the submitted cell coordinates.
/// The worker's `LINE` pragmas already use `<cell>`, so no generated-wrapper
/// offset is involved.
#[must_use]
pub fn render_cell_compile_error(error: &CompileError, cell_text: &str) -> String {
    render_cell_compile_rejection(error, cell_text).output
}

/// [`render_cell_compile_error`]'s text plus its diagnostics as data.
#[must_use]
pub fn render_cell_compile_rejection(error: &CompileError, cell_text: &str) -> CompileRejection {
    let CompileError::Diagnostics(diagnostics) = error else {
        return CompileRejection::unstructured(crate::classify_compile(error).message);
    };
    let rendered = crate::diag::render_diagnostics_structured(
        diagnostics,
        &crate::diag::RenderOpts {
            anchor: "<cell>",
            label: "<cell>",
            user_lines: None,
            line_offset: 0,
            col_indent: 0,
            drop_foreign_gen_warnings_except: None,
            source: cell_text,
        },
    );
    CompileRejection {
        output: with_advice(rendered.text, |text| advice_for(text, cell_text)),
        diagnostics: rendered.diagnostics,
    }
}

/// Whether GHC's own text is worth keeping beside the advice.
///
/// Both answers are right somewhere. When the diagnostic is about the reader's
/// code — a named binding whose constraint stayed open, an ambiguous literal,
/// a field that two cell generations both define — GHC says which binding and
/// which constraint, and the advice is only a sentence about what to do with
/// that. Replacing it left eight cells across two dogfood runs told to add a
/// signature with no way to know what to write.
///
/// When the diagnostic is an artifact of how a cell is wrapped — a `ZonkAny`
/// standing in for a type the reader never named, or an overlap that hangs on
/// an uninstantiated variable — its text names internals from the generated
/// module, and keeping it only invites chasing them. Those are replaced.
enum Diagnostic {
    /// About the submitted code: keep it, and add the advice after it.
    WorthReading,
    /// About the wrapper: the advice is the whole of what can be acted on.
    Artifact,
}

fn with_advice(
    rendered: String,
    advise: impl FnOnce(&str) -> Option<(String, Diagnostic)>,
) -> String {
    match advise(&rendered) {
        None => rendered,
        Some((advice, Diagnostic::Artifact)) => advice,
        Some((advice, Diagnostic::WorthReading)) if rendered.contains(&advice) => rendered,
        Some((advice, Diagnostic::WorthReading)) => format!("{rendered}\n\n{advice}"),
    }
}

/// Types a cell cannot write as a literal, and how to build one.
///
/// Passing the wrong shape was the largest single cost across four dogfood
/// runs and did not fall between them, because the diagnostic says what was
/// expected without saying how to make one. Where a literal works the instance
/// is the fix — `GitRef` and `BranchName` took `IsString` and the whole class
/// went away — but a fork group path is a campaign and a group, so no literal
/// can mean it and naming the constructor is the fix instead.
const CONSTRUCTORS: &[(&str, &str)] = &[
    (
        "ForkGroupPath",
        "a fork group path is a campaign and a group, so no string literal can name one:          build it with `batch \"campaign\" \"group\"`",
    ),
    (
        "WorktreeSpec",
        "build a worktree spec with `fromRef ref label`, `fromCurrentRepository label`,          or `fromWorktree id label`",
    ),
];

/// Name the constructor for a type the cell tried to write directly.
#[must_use]
pub fn constructor_advice(message: &str) -> Option<String> {
    let names_expected = |name: &str| {
        message.contains(&format!("expected type: {name}"))
            || message.contains(&format!("expected type ‘{name}’"))
            || message.contains(&format!("expected type `{name}'"))
            || message.contains(&format!("IsString {name}"))
            || message.contains(&format!("IsString ‘{name}’"))
    };
    CONSTRUCTORS
        .iter()
        .find(|(name, _)| names_expected(name))
        .map(|(_, advice)| (*advice).to_owned())
}

/// [`ambiguous_type_advice`] plus whether the diagnostic it recognised is one
/// the reader should still see.
fn advice_for(message: &str, submitted: &str) -> Option<(String, Diagnostic)> {
    if let Some(advice) = constructor_advice(message) {
        return Some((advice, Diagnostic::WorthReading));
    }
    let advice = ambiguous_type_advice(message, submitted)?;
    let artifact = message.contains("ZonkAny")
        || (message.contains("Overlapping instances for")
            && message.contains("The choice depends on the instantiation of"));
    Some((
        advice,
        if artifact {
            Diagnostic::Artifact
        } else {
            Diagnostic::WorthReading
        },
    ))
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
    /// The turn's prepared-STG program, when the request asked for one.
    pub prepared: Option<PreparedProgram>,
}

impl CompiledTurn {
    /// Borrow the halves a resident session runs.
    #[must_use]
    pub fn code(&self) -> TurnCode<'_> {
        TurnCode {
            expr: &self.expr,
            table: &self.table,
            sites: &self.asks,
            prepared: self.prepared.as_ref(),
        }
    }
}

/// The compiled halves of one turn a resident session may run. The Core
/// half (`expr`) is what every turn compiles today; `prepared` is the program
/// a prepared-route turn adds. The session's engine, fixed at construction,
/// runs its own half and never the other; `table` and `sites` describe both
/// (one constructor table serves both engines).
#[derive(Clone, Copy)]
pub struct TurnCode<'a> {
    pub expr: &'a CoreExpr,
    pub table: &'a DataConTable,
    pub sites: &'a [YieldSite],
    pub prepared: Option<&'a PreparedProgram>,
}

impl<'a> TurnCode<'a> {
    /// A Core-only turn: what a caller without a [`CompiledTurn`] (a hand-built
    /// fragment, a fixture) runs. On the prepared route it is refused, never
    /// run on Core.
    #[must_use]
    pub fn core(expr: &'a CoreExpr, table: &'a DataConTable, sites: &'a [YieldSite]) -> Self {
        Self {
            expr,
            table,
            sites,
            prepared: None,
        }
    }
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
    pub source: DeclarationSource,
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
    let mut out = with_resume_import(preamble_with_imports);
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
    out.push_str(&prepared_scaffold_binding(target));
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
            &with_resume_import(preamble_with_imports),
            "qualified GHC.TypeError as TidepoolWorkbenchTypeError",
        )
    } else {
        with_resume_import(preamble_with_imports)
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
    out.push_str(&prepared_scaffold_binding(target));
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
///
/// Explicit braces disable GHC's layout algorithm entirely, so a multi-line
/// `let` group (a signature on one line and its equation indented under it,
/// or several equations at the same column) needs the `;` layout would have
/// inserted between its items reproduced by hand — [`explicit_brace_let_body`]
/// does that by comparing each continuation line's indentation against the
/// first item's column, the same reference column GHC's own layout rule uses.
pub fn place_turn_stmt(turn_text: &str) -> String {
    let trimmed = turn_text.trim_start();
    let let_rest = trimmed
        .strip_prefix("let")
        .filter(|r| r.starts_with(|c: char| c.is_whitespace()));
    match let_rest {
        Some(rest) if !rest.trim_start().starts_with('{') => {
            let body = explicit_brace_let_body(rest);
            let mut out = String::from("let {");
            out.push_str(&body);
            if !body.ends_with('\n') {
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

/// Insert the `;` GHC's layout rule would have placed between items of a
/// `let` group whose text (`rest`, everything after the `let` keyword) spans
/// more than one line. The reference column is the column of the group's
/// first token — 1-based, counting the 3 characters of `let` itself, since a
/// raw turn has no leading indentation. A later line starting exactly at that
/// column begins a new item in the group (layout would close the previous
/// item and open the next with `;`); a line indented further is a
/// continuation of the item above it (part of the same equation or a
/// multi-line expression) and is left untouched. Only whitespace characters
/// are inserted or removed nowhere — one `;` is spliced into an existing
/// line — so the line count, and therefore [`turn_user_code_line_range`]'s
/// offset math over the unmodified `turn_text`, is unaffected.
fn explicit_brace_let_body(rest: &str) -> String {
    let first_line = rest.split('\n').next().unwrap_or("");
    let first_nonws = first_line
        .char_indices()
        .find(|(_, character)| !character.is_whitespace())
        .map_or(first_line.len(), |(index, _)| index);
    // 3 == "let".len(); +1 converts the 0-based byte offset to a 1-based column.
    let reference_column = 3 + first_nonws + 1;

    let mut out = String::with_capacity(rest.len() + 8);
    for (index, line) in rest.split('\n').enumerate() {
        if index > 0 {
            out.push('\n');
        }
        let indent = line.len() - line.trim_start().len();
        if index > 0 && !line.trim().is_empty() && indent + 1 == reference_column {
            out.push_str(&line[..indent]);
            out.push(';');
            out.push_str(&line[indent..]);
        } else {
            out.push_str(line);
        }
    }
    out
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
pub fn check_cell(req: CellCheckRequest<'_>) -> Result<CellCheck, CellCheckFailure> {
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
        let items = match std::fs::read(&out_path) {
            Ok(bytes) => Some(decode_cell_out(&bytes)?.items),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        return Err(CellCheckFailure { error, items });
    }
    let bytes = std::fs::read(&out_path)?;
    decode_cell_out(&bytes).map_err(Into::into)
}

/// [`check_cell`], but a cell whose only problem is
/// [`is_cell_pure_dispatch_ambiguity`] is admitted rather than rejected.
///
/// `TidepoolCellExpression`'s OVERLAPPING `Eff effects value` head and
/// OVERLAPPABLE bare-`value` head cannot be arranged to prefer the effectful
/// reading themselves: GHC's overlap resolution always settles an ambiguous
/// choice on the unconditionally-matching head (bare `value`) once forced to
/// pick, which is backwards from what a cell ending in `pure <expr>` wants,
/// and no combination of `OVERLAPPING`/`OVERLAPPABLE`/`INCOHERENT` pragmas
/// changes which side wins — only which side is allowed to win silently.
/// So instead: on exactly this diagnostic, retry once with the cell's final
/// expression wrapped in `__tidepoolInEffectRow` (the check template defines
/// it — see [`super::workbench::resident_cell_check_template`]), which pins
/// the ambiguous metavariable to the workbench's own effect row before
/// `TidepoolCellExpression` ever has to choose. A genuinely pure final
/// expression (its own concrete, non-`Eff` type) fails that pin and keeps
/// today's behavior; a genuine type error never carries this diagnostic
/// shape, so it costs the one compile [`check_cell`] always cost.
///
/// The retry is a scratch compile only: on success, the returned
/// [`CellCheck`]'s final item keeps the author's own source and span, not the
/// pinned scratch text, so a per-unit compile downstream still runs the
/// unmodified turn through the unrelated, already-correct
/// [`ExpressionLift::Effectful`]-then-[`ExpressionLift::Pure`] selection
/// (`assemble_display_expression_module` et al.) — this function only gets
/// the cell admitted, and does not decide how the final unit actually runs.
pub fn check_cell_preferring_effectful(
    req: CellCheckRequest<'_>,
) -> Result<CellCheck, CellCheckFailure> {
    let CellCheckRequest {
        cell_text,
        template,
        include,
        session_root,
        inject_modules,
    } = req;
    let failure = match check_cell(CellCheckRequest {
        cell_text,
        template,
        include,
        session_root,
        inject_modules,
    }) {
        Ok(checked) => return Ok(checked),
        Err(failure) => failure,
    };
    let Some((retry_text, original_final_item)) = pin_final_cell_expression(&failure, cell_text)
    else {
        return Err(failure);
    };
    match check_cell(CellCheckRequest {
        cell_text: &retry_text,
        template,
        include,
        session_root,
        inject_modules,
    }) {
        Ok(mut checked) => {
            // The pin exists only to get the preflight past the ambiguity;
            // restore the author's own text/span so nothing downstream (a
            // per-unit compile, a receipt echoed back to the reader) ever
            // sees the scratch wrapper this function invented.
            if let Some(slot) = checked.items.last_mut() {
                *slot = original_final_item;
            }
            Ok(checked)
        }
        // The pinned retry's diagnostics are anchored to scratch text the
        // reader never wrote; report the original failure, whose spans still
        // match `cell_text`.
        Err(_) => Err(failure),
    }
}

/// Build a scratch copy of `cell_text` with its final item's expression
/// wrapped in `__tidepoolInEffectRow`, alongside that item exactly as
/// originally classified — for [`check_cell_preferring_effectful`] to restore
/// after a successful pinned retry. `None` unless `failure` is exactly
/// [`is_cell_pure_dispatch_ambiguity`]'s shape, classification produced at
/// least one item, and that final item is a bare expression whose source GHC
/// reported can still be found verbatim in `cell_text` (it always can — see
/// [`CellAnalysisItem::source`] — this is a defensive `None`, not an expected
/// one).
fn pin_final_cell_expression(
    failure: &CellCheckFailure,
    cell_text: &str,
) -> Option<(String, CellAnalysisItem)> {
    let envelope = crate::classify_compile(&failure.error);
    if envelope.class != crate::FailureClass::UserHaskell
        || !is_cell_pure_dispatch_ambiguity(&envelope.message)
    {
        return None;
    }
    let last = failure.items.as_ref()?.last()?;
    if last.verdict.kind != TurnKind::Expr {
        return None;
    }
    let start = cell_text.rfind(last.source.as_str())?;
    let end = start + last.source.len();
    let mut retry_text = String::with_capacity(cell_text.len() + 32);
    retry_text.push_str(&cell_text[..start]);
    retry_text.push_str("__tidepoolInEffectRow (\n");
    retry_text.push_str(&cell_text[start..end]);
    retry_text.push_str("\n)\n");
    retry_text.push_str(&cell_text[end..]);
    Some((retry_text, last.clone()))
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
    if let Some(prepared) = &req.prepared {
        cmd.prepared_turn();
        for (identity, generation) in prepared.retained {
            cmd.retained_generation(extract_identity(identity), *generation);
        }
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

    decode_turn_output_dir(temp.path(), req.prepared.is_some()).map_err(Into::into)
}

/// Decode one item's full output directory into a [`TurnResult`]: the
/// `TurnOut` CBOR sidecar (`turn.cbor`) plus, for a `Bind`/`Expr` verdict,
/// `result.cbor`/`meta.cbor` off the SAME directory.
fn decode_turn_output_dir(dir: &Path, prepared: bool) -> Result<TurnResult, CompileError> {
    let turn_out_path = dir.join("turn.cbor");
    if !turn_out_path.exists() {
        return Err(CompileError::MissingOutput(turn_out_path));
    }
    let turn_out_bytes = std::fs::read(&turn_out_path)?;
    let turn_out = decode_turn_out(&turn_out_bytes)?;

    match turn_out {
        DecodedTurnOut::Decl {
            binders,
            items,
            source,
        } => Ok(TurnResult::Decl(DeclarationReceipt {
            binders,
            items,
            source,
        })),
        DecodedTurnOut::Bind {
            binders,
            variant,
            bound,
            asks,
            wrapped_source,
        } => {
            let compiled = read_compiled_turn(dir, asks, prepared)?;
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
            let compiled = read_compiled_turn(dir, asks, prepared)?;
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
    prepared: bool,
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

    let prepared = prepared
        .then(|| read_prepared_program(output_dir))
        .transpose()?;

    Ok(CompiledTurn {
        expr,
        table,
        warnings,
        asks,
        prepared,
    })
}

/// Read the prepared-STG program the worker wrote beside this turn's Core
/// artifacts. A requested program that is absent is a missing output, never a
/// silent Core-only turn.
fn read_prepared_program(output_dir: &Path) -> Result<PreparedProgram, CompileError> {
    let path = output_dir.join(format!("{PREPARED_SCAFFOLD_TARGET}.prepared.cbor"));
    if !path.exists() {
        return Err(CompileError::MissingOutput(path));
    }
    let prepared_read_start = std::time::Instant::now();
    let bytes = std::fs::read(&path)?;
    let prepared_read_bytes = bytes.len() as u64;
    timing::record_stage(
        timing::NO_NODE,
        timing::NO_ROUND,
        timing::STAGE_PREPARED_READ,
        prepared_read_start.elapsed(),
        prepared_read_bytes,
    );
    let requirements = tidepool_toolchain::prepared_artifact::production_requirements()
        .map_err(|error| CompileError::ExtractFailed(error.to_string()))?;
    parse_program(&bytes, &requirements, DecodeLimits::default())
        .map_err(|error| CompileError::ExtractFailed(format!("{path:?}: {error}")))
}

/// The decoded shape of the `TurnOut` CBOR sidecar, before `run_turn` reads
/// `result.cbor`/`meta.cbor` to build the final [`CompiledTurn`].
#[derive(Debug)]
enum DecodedTurnOut {
    Decl {
        binders: Vec<String>,
        items: Vec<ExportItem>,
        source: DeclarationSource,
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

fn decode_source_prologue(value: &CborValue) -> Result<SourcePrologue, CompileError> {
    let fields = cbor_expect_array_len(value, 2, "source prologue")?;
    let pragmas = cbor_expect_array(&fields[0], "source pragmas")?
        .iter()
        .map(|value| {
            let fields = cbor_expect_array_len(value, 3, "source pragma")?;
            let kind = match cbor_expect_text(&fields[0], "pragma kind")? {
                "language" => PragmaKind::Language,
                "options_ghc" => PragmaKind::OptionsGhc,
                other => {
                    return Err(CompileError::ExtractFailed(format!(
                        "unknown source pragma kind {other:?}"
                    )))
                }
            };
            Ok(LocatedPragma {
                kind,
                span: decode_cell_span(&fields[1])?,
                source: cbor_expect_text(&fields[2], "pragma source")?.to_owned(),
            })
        })
        .collect::<Result<Vec<_>, CompileError>>()?;
    let imports = cbor_expect_array(&fields[1], "source imports")?
        .iter()
        .map(|value| {
            let fields = cbor_expect_array_len(value, 2, "source import")?;
            let source = cbor_expect_text(&fields[1], "import source")?;
            if !source.starts_with("import ") || source.contains(['\n', '\r']) {
                return Err(CompileError::ExtractFailed(
                    "worker import is not a normalized single-line declaration".into(),
                ));
            }
            Ok(LocatedImport {
                span: decode_cell_span(&fields[0])?,
                source: source.to_owned(),
            })
        })
        .collect::<Result<Vec<_>, CompileError>>()?;
    Ok(SourcePrologue { pragmas, imports })
}

fn decode_declaration_source(value: &CborValue) -> Result<DeclarationSource, CompileError> {
    let fields = cbor_expect_array_len(value, 2, "declaration source")?;
    Ok(DeclarationSource {
        prologue: decode_source_prologue(&fields[0])?,
        body: cbor_expect_text(&fields[1], "declaration body")?.to_owned(),
    })
}

fn decode_cell_out(bytes: &[u8]) -> Result<CellCheck, CompileError> {
    let value: CborValue = ciborium::de::from_reader(bytes).map_err(|error| {
        CompileError::ExtractFailed(format!("CellOut CBOR: malformed: {error}"))
    })?;
    let root = cbor_expect_array_len(&value, 4, "CellOut")?;
    let items = cbor_expect_array(&root[0], "cell items")?
        .iter()
        .map(decode_cell_item)
        .collect::<Result<Vec<_>, _>>()?;
    validate_cell_source_items(&items)?;
    let pins = cbor_expect_array(&root[1], "cell pins")?
        .iter()
        .map(decode_checked_binder_pin)
        .collect::<Result<Vec<_>, _>>()?;
    let checked_source = cbor_expect_text(&root[2], "checked cell source")?.to_owned();
    Ok(CellCheck {
        items,
        pins,
        checked_source,
        prologue: decode_source_prologue(&root[3])?,
    })
}

fn validate_cell_source_items(items: &[CellAnalysisItem]) -> Result<(), CompileError> {
    if items.iter().any(|item| item.source_items.is_empty()) {
        return Err(CompileError::ExtractFailed(
            "CellOut CBOR: execution item has no source items".into(),
        ));
    }
    if items.iter().any(|item| {
        item.source_items
            .iter()
            .any(|source| source.kind != item.verdict.kind)
    }) {
        return Err(CompileError::ExtractFailed(
            "CellOut CBOR: source item kind does not match its execution item".into(),
        ));
    }
    let mut ordinals = items
        .iter()
        .flat_map(|item| item.source_items.iter().map(|source| source.ordinal))
        .collect::<Vec<_>>();
    ordinals.sort_unstable();
    if ordinals.iter().copied().ne(0..ordinals.len()) {
        return Err(CompileError::ExtractFailed(
            "CellOut CBOR: source item ordinals are not contiguous and unique".into(),
        ));
    }
    Ok(())
}

fn decode_cell_span(value: &CborValue) -> Result<CellSourceSpan, CompileError> {
    let span = cbor_expect_array_len(value, 4, "source span")?;
    Ok(CellSourceSpan {
        start_line: cbor_as_usize(&span[0], "span start line")?,
        start_column: cbor_as_usize(&span[1], "span start column")?,
        end_line: cbor_as_usize(&span[2], "span end line")?,
        end_column: cbor_as_usize(&span[3], "span end column")?,
    })
}

fn decode_cell_item(value: &CborValue) -> Result<CellAnalysisItem, CompileError> {
    let fields = cbor_expect_array_len(value, 5, "cell item")?;
    let span = decode_cell_span(&fields[0])?;
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
        source_items: cbor_expect_array(&fields[4], "cell source items")?
            .iter()
            .map(decode_cell_source_item)
            .collect::<Result<Vec<_>, _>>()?,
    })
}

fn decode_cell_source_item(value: &CborValue) -> Result<CellAnalysisSourceItem, CompileError> {
    let fields = cbor_expect_array_len(value, 3, "cell source item")?;
    let kind = match cbor_expect_text(&fields[2], "cell source item kind")? {
        "decl" => TurnKind::Decl,
        "bind" => TurnKind::Bind,
        "expr" => TurnKind::Expr,
        other => {
            return Err(CompileError::ExtractFailed(format!(
                "CellOut CBOR: unknown cell source item kind {other:?}"
            )))
        }
    };
    Ok(CellAnalysisSourceItem {
        ordinal: cbor_as_usize(&fields[0], "cell source item ordinal")?,
        span: decode_cell_span(&fields[1])?,
        kind,
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
            let payload = cbor_expect_array_len(&root[1], 3, "Decl payload")?;
            let binders = decode_string_array(&payload[0], "Decl binders")?;
            let items = decode_export_items(&payload[1])?;
            Ok(DecodedTurnOut::Decl {
                binders,
                items,
                source: decode_declaration_source(&payload[2])?,
            })
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
mod ambiguity_advice_tests {
    use super::{
        ambiguous_type_advice, constructor_advice, is_cell_pure_dispatch_ambiguity,
        pin_final_cell_expression, refutable_binds, render_cell_compile_error,
        runtime_failure_advice, same_cell_value_collisions, CellAnalysisItem,
        CellAnalysisSourceItem, CellCheckFailure, CellSourceSpan, TurnClassification, TurnKind,
        AMBIGUOUS_TYPE_ADVICE, CELL_PURE_DISPATCH_ADVICE, LITERAL_ANNOTATION_ADVICE,
        SPLIT_SIGNATURE_ADVICE,
    };
    use crate::CompileError;

    /// Exactly the GHC texts observed in dogfood runs 4 and 5, not paraphrases:
    /// a replanted binder type that leaked the zonker's own name, a displayed
    /// tuple whose element stayed a higher-kinded variable, and a bare
    /// `error "..."` cell whose only constraint is the check template's own
    /// expression class.
    const ZONK_ANY: &str = "<cell>:1:1: error: [GHC-76037]\n    Not in scope: type constructor or class \u{2018}GHC.Types.ZonkAny\u{2019}";
    const HIGHER_KINDED_RENDER: &str = "<cell>:3:5: error: [GHC-43085]\n    \u{2022} Overlapping instances for Render (f0 Double)\n        arising from a use of \u{2018}render\u{2019}\n      Matching instance:\n        instance [overlappable] Show a => Render a -- Defined in \u{2018}Tidepool.Render\u{2019}\n      (The choice depends on the instantiation of \u{2018}f0\u{2019}\n       To pick the first instance above, use IncoherentInstances\n       when compiling the other instance declarations)";
    const BARE_ERROR_CELL: &str = "<cell>:1:1: error: [GHC-43085]\n    \u{2022} Overlapping instances for TidepoolCellExpression value0\n        arising from a use of \u{2018}__tidepoolCellExpression\u{2019}\n      Matching instances:\n        instance [overlappable] TidepoolCellPure value => TidepoolCellExpression value\n        instance [overlapping] (effects ~ ActorEffects) => TidepoolCellExpression (Eff effects value)\n      (The choice depends on the instantiation of \u{2018}value0\u{2019}\n       To pick the first instance above, use IncoherentInstances\n       when compiling the other instance declarations)";

    fn cell_error(message: &str) -> CompileError {
        CompileError::Diagnostics(vec![crate::diag::ExtractDiag {
            span: None,
            severity: crate::diag::DiagnosticSeverity::Error,
            message: message.to_owned(),
        }])
    }

    /// A cell rejection hands over both forms at once: the rendered text
    /// unchanged, and the same diagnostics as data — including the one GHC
    /// gave no span for, which stays representable rather than being dropped
    /// or given a coordinate it never had.
    #[test]
    fn cell_rejection_carries_the_same_spans_as_its_rendered_text() {
        use crate::diag::{DiagnosticLevel, DiagnosticLocation};
        let cell_text = "let x = 1\n  missing thing\n";
        let error = CompileError::Diagnostics(vec![
            crate::diag::ExtractDiag {
                span: Some(crate::diag::DiagSpan {
                    file: "<cell>".into(),
                    start_line: 2,
                    start_col: 3,
                    end_line: 2,
                    end_col: 10,
                }),
                severity: crate::diag::DiagnosticSeverity::Error,
                message: "Variable not in scope: missing".into(),
            },
            crate::diag::ExtractDiag {
                span: None,
                severity: crate::diag::DiagnosticSeverity::Warning,
                message: "compiler worker stderr: no location".into(),
            },
        ]);
        let rejection = super::render_cell_compile_rejection(&error, cell_text);

        // 1. The rendered text is exactly what the existing entry point
        //    already returns — same function, same bytes.
        assert_eq!(
            rejection.output,
            render_cell_compile_error(&error, cell_text)
        );
        assert!(
            rejection
                .output
                .starts_with("<cell>:2:3-10: error:\n    Variable not in scope: missing"),
            "{}",
            rejection.output
        );

        // 2. The structure says the same thing, without anyone parsing it
        //    back out of that header.
        assert_eq!(
            rejection.diagnostics.len(),
            2,
            "{:?}",
            rejection.diagnostics
        );
        assert_eq!(rejection.diagnostics[0].severity, DiagnosticLevel::Error);
        assert_eq!(
            rejection.diagnostics[0].location,
            DiagnosticLocation::Authored {
                label: "<cell>".into(),
                start_line: 2,
                start_col: 3,
                end_line: 2,
                end_col: 10,
            }
        );
        assert_eq!(
            rejection.diagnostics[0].message,
            "Variable not in scope: missing"
        );

        // 3. The unspanned one survives whole.
        assert_eq!(rejection.diagnostics[1].severity, DiagnosticLevel::Warning);
        assert_eq!(
            rejection.diagnostics[1].location,
            DiagnosticLocation::Unlocated
        );
        assert_eq!(
            rejection.diagnostics[1].message,
            "compiler worker stderr: no location"
        );
    }

    /// A rejection that is not a GHC diagnostics report has no structure to
    /// offer and says so, rather than manufacturing an entry. The rendered
    /// message is unchanged.
    #[test]
    fn a_non_diagnostic_compile_failure_renders_as_before_with_no_structure() {
        let error = CompileError::MalformedDiagnostics("stale deployed extract-bin".into());
        let rejection = super::render_cell_compile_rejection(&error, "x = 1");
        assert_eq!(rejection.output, render_cell_compile_error(&error, "x = 1"));
        assert!(rejection.diagnostics.is_empty());
    }

    /// The live diagnostic reproduced against a real resident cell whose
    /// final unit is `pure (1 :: Int)`. Distinct from [`BARE_ERROR_CELL`]
    /// (also a `TidepoolCellExpression` overlap, but over a fully polymorphic
    /// `error "…"` with no `Applicative`/`Monad` constraint left open) —
    /// only this shape is [`check_cell_preferring_effectful`]'s to fix.
    const AMBIGUOUS_PURE_DISPATCH: &str = "<cell>:1:1: error: [GHC-39999]\n    \u{2022} Ambiguous type variable \u{2018}f0\u{2019} arising from a use of \u{2018}pure\u{2019}\n      prevents the constraint \u{2018}(Applicative f0)\u{2019} from being solved.\n      Probable fix: use a type annotation to specify what \u{2018}f0\u{2019} should be.";

    #[test]
    fn pure_dispatch_ambiguity_is_recognized_narrowly() {
        assert!(is_cell_pure_dispatch_ambiguity(AMBIGUOUS_PURE_DISPATCH));
        // Every other ambiguity family in this module — including the other
        // `TidepoolCellExpression` overlap, which carries no
        // Applicative/Monad constraint — is left alone.
        for message in [ZONK_ANY, HIGHER_KINDED_RENDER, BARE_ERROR_CELL] {
            assert!(
                !is_cell_pure_dispatch_ambiguity(message),
                "unrelated ambiguity misclassified as a pure/effect dispatch: {message}"
            );
        }
    }

    /// The whole point of the new advice: no signature repairs this, so the
    /// reader is told to drop `pure`/`return` instead — and only when a
    /// genuine error survives [`check_cell_preferring_effectful`]'s pinned
    /// retry does this text ever reach anyone (see that function's own
    /// tests for the common case, where the cell is admitted and no advice
    /// is shown at all).
    #[test]
    fn a_pure_dispatch_ambiguity_is_told_to_drop_pure_not_add_a_signature() {
        assert_eq!(
            ambiguous_type_advice(AMBIGUOUS_PURE_DISPATCH, "pure (1 :: Int)").as_deref(),
            Some(CELL_PURE_DISPATCH_ADVICE)
        );
        let rendered = render_cell_compile_error(&cell_error(AMBIGUOUS_PURE_DISPATCH), "pure (1 :: Int)");
        assert!(
            rendered.contains("Ambiguous type variable"),
            "GHC's own text must survive: {rendered}"
        );
        assert!(rendered.ends_with(CELL_PURE_DISPATCH_ADVICE), "{rendered}");
    }

    #[test]
    fn pin_final_cell_expression_wraps_only_the_final_expression_item() {
        let bind_item = CellAnalysisItem {
            span: CellSourceSpan { start_line: 1, start_column: 1, end_line: 1, end_column: 12 },
            source: "h <- pure 1".to_owned(),
            verdict: TurnClassification {
                kind: TurnKind::Bind,
                binders: vec!["h".to_owned()],
                items: Vec::new(),
            },
            source_items: vec![CellAnalysisSourceItem {
                ordinal: 0,
                span: CellSourceSpan { start_line: 1, start_column: 1, end_line: 1, end_column: 12 },
                kind: TurnKind::Bind,
            }],
        };
        let final_item = CellAnalysisItem {
            span: CellSourceSpan { start_line: 2, start_column: 1, end_line: 2, end_column: 16 },
            source: "pure (1 :: Int)".to_owned(),
            verdict: TurnClassification {
                kind: TurnKind::Expr,
                binders: Vec::new(),
                items: Vec::new(),
            },
            source_items: vec![CellAnalysisSourceItem {
                ordinal: 1,
                span: CellSourceSpan { start_line: 2, start_column: 1, end_line: 2, end_column: 16 },
                kind: TurnKind::Expr,
            }],
        };
        let failure = CellCheckFailure {
            error: cell_error(AMBIGUOUS_PURE_DISPATCH),
            items: Some(vec![bind_item.clone(), final_item.clone()]),
        };
        let cell_text = "h <- pure 1\npure (1 :: Int)\n";
        let (retry_text, restored) =
            pin_final_cell_expression(&failure, cell_text).expect("this is exactly the pinnable shape");
        assert_eq!(restored.source, final_item.source);
        assert_eq!(restored.span, final_item.span);
        assert!(
            retry_text.starts_with("h <- pure 1\n__tidepoolInEffectRow (\npure (1 :: Int)\n)"),
            "only the final item's own text is pinned, in place: {retry_text}"
        );

        // A genuine type error never carries this diagnostic shape, so it is
        // never pinned — one compile, not two.
        let ordinary_error = cell_error(
            "<cell>:1:1: error: [GHC-83865]\n    \u{2022} Couldn't match type \u{2018}Int\u{2019} with \u{2018}Text\u{2019}",
        );
        let ordinary_failure = CellCheckFailure {
            error: ordinary_error,
            items: Some(vec![bind_item, final_item]),
        };
        assert!(pin_final_cell_expression(&ordinary_failure, cell_text).is_none());
    }

    #[test]
    fn every_unresolved_type_variable_shape_asks_for_a_signature() {
        for message in [ZONK_ANY, HIGHER_KINDED_RENDER, BARE_ERROR_CELL] {
            assert_eq!(
                ambiguous_type_advice(message, "firstReview = \\(a,_,_) -> a").as_deref(),
                Some(AMBIGUOUS_TYPE_ADVICE),
                "unresolved-type-variable diagnostic was not recognized: {message}"
            );
            assert_eq!(
                render_cell_compile_error(&cell_error(message), "firstReview = \\(a,_,_) -> a"),
                AMBIGUOUS_TYPE_ADVICE,
                "the model-facing cell message still carried GHC's text"
            );
        }
    }

    /// GHC's own ambiguity family, as run 6 hit it four times on one helper.
    /// The advice names the binding and says where its signature goes for the
    /// form the model actually wrote.
    const AMBIGUOUS_TOP_LEVEL: &str = "<cell>:2:16: error: [GHC-01928]\n    \u{2022} Ambiguous type variable \u{2018}a0\u{2019} arising from a use of \u{2018}render\u{2019}\n      prevents the constraint \u{2018}(Render a0)\u{2019} from being solved.\n    \u{2022} In an equation for \u{2018}summarize\u{2019}:\n          summarize xs = render (head xs)";
    const AMBIGUOUS_FIND_ELEM: &str = "<cell>:1:9: error: [GHC-01928]\n    \u{2022} Ambiguous type variable \u{2018}effects0\u{2019} arising from a use of \u{2018}say\u{2019}\n      prevents the constraint \u{2018}(FindElem Say effects0)\u{2019} from being solved.\n    \u{2022} In an equation for \u{2018}announce\u{2019}: announce message = say message";

    #[test]
    fn an_ambiguous_binding_is_named_with_its_signature_placement() {
        assert_eq!(
            ambiguous_type_advice(AMBIGUOUS_TOP_LEVEL, "summarize xs = render (head xs)").unwrap(),
            "`summarize`'s type is ambiguous; add its signature line directly above the \
             equation in the same cell item: `summarize :: T -> U`"
        );
        // Advice is added to the diagnostic, never in place of it: the
        // reader needs GHC's own account of which constraint was left open in
        // order to know what signature to write.
        let rendered = render_cell_compile_error(
            &cell_error(AMBIGUOUS_TOP_LEVEL),
            "summarize xs = render (head xs)",
        );
        assert!(rendered.contains("Ambiguous type variable"), "{rendered}");
        assert!(
            rendered.ends_with(
                "`summarize`'s type is ambiguous; add its signature line directly above the \
                 equation in the same cell item: `summarize :: T -> U`"
            ),
            "{rendered}"
        );
        // A handler helper that never got its effect row lands here too.
        assert!(ambiguous_type_advice(AMBIGUOUS_FIND_ELEM, "announce message = say message")
            .unwrap()
            .starts_with("`announce`'s type is ambiguous"));
    }

    #[test]
    fn a_let_bound_binding_keeps_its_signature_on_the_same_binding() {
        let cell = "do\n  let summarize xs = render (head xs)\n  say (summarize items)";
        assert_eq!(
            ambiguous_type_advice(AMBIGUOUS_TOP_LEVEL, cell).unwrap(),
            "`summarize`'s type is ambiguous; give it a signature in the same `let` binding: \
             `let summarize :: T -> U; summarize x = …` (a signature on its own `let` line \
             loses the argument scope)"
        );
        let block = "do\n  let\n    summarize xs = render (head xs)\n  say (summarize items)";
        assert_eq!(
            ambiguous_type_advice(AMBIGUOUS_TOP_LEVEL, block),
            ambiguous_type_advice(AMBIGUOUS_TOP_LEVEL, cell)
        );
        // A name that merely shares a prefix with the `let` binding is not it.
        let other = "do\n  let summarizeAll xs = render xs\n  say (summarize items)";
        assert!(ambiguous_type_advice(AMBIGUOUS_TOP_LEVEL, other)
            .unwrap()
            .contains("directly above the"));
    }

    /// A bare literal under `ToJSON`/`IsString`: no declaration signature
    /// repairs it, so the advice points at the literal.
    #[test]
    fn an_ambiguous_literal_asks_for_an_annotation_at_the_literal() {
        let message = "<cell>:1:24: error: [GHC-01928]\n    \u{2022} Ambiguous type variable \u{2018}a0\u{2019} arising from the literal \u{2018}\"src/app.rs\"\u{2019}\n      prevents the constraint \u{2018}(Data.String.IsString a0)\u{2019} from being solved.\n    \u{2022} In the first argument of \u{2018}toJSON\u{2019}, namely \u{2018}\"src/app.rs\"\u{2019}";
        assert_eq!(
            ambiguous_type_advice(message, "value = toJSON \"src/app.rs\"").as_deref(),
            Some(LITERAL_ANNOTATION_ADVICE)
        );
        let rendered = render_cell_compile_error(&cell_error(message), "value = toJSON \"src/app.rs\"");
        assert!(rendered.contains("arising from the literal"), "{rendered}");
        assert!(rendered.ends_with(LITERAL_ANNOTATION_ADVICE), "{rendered}");
    }

    /// Each cell item compiles alone, so a signature whose equation went into
    /// the next item installs nothing and the next item reports the name as
    /// out of scope. The first item says so.
    #[test]
    fn a_signature_without_its_equation_says_they_share_one_item() {
        let message = "<cell>:1:1: error: [GHC-44432]\n    The type signature for \u{2018}summarize\u{2019} lacks an accompanying binding";
        let rendered = render_cell_compile_error(&cell_error(message), "summarize :: [Text] -> Text");
        assert!(rendered.contains("lacks an accompanying binding"), "{rendered}");
        assert!(
            rendered.ends_with(&format!(
                "`summarize` has a signature but no equation in this cell item; \
                 {SPLIT_SIGNATURE_ADVICE}"
            )),
            "{rendered}"
        );
    }

    /// A ground overlap is a real instance conflict the author must resolve,
    /// and an ordinary mismatch already names the two types. Neither is
    /// repaired by a signature, so neither is rewritten.
    #[test]
    fn diagnostics_that_name_concrete_types_are_left_alone() {
        let ground_overlap = "<cell>:1:1: error: [GHC-43085]\n    \u{2022} Overlapping instances for Render Text\n      Matching instances:\n        instance Render Text\n        instance [overlappable] Show a => Render a";
        let mismatch = "<cell>:1:1: error: [GHC-83865]\n    \u{2022} Couldn't match type \u{2018}Int\u{2019} with \u{2018}Text\u{2019}";
        for message in [ground_overlap, mismatch] {
            assert_eq!(ambiguous_type_advice(message, "x = 1"), None);
            let rendered = render_cell_compile_error(&cell_error(message), "x = 1");
            assert_ne!(rendered, AMBIGUOUS_TYPE_ADVICE);
            assert!(
                rendered.contains("Render Text") || rendered.contains("Couldn't match type"),
                "GHC's own text must survive: {rendered}"
            );
        }
    }

    /// The exact GHC text observed against a live session (2026-09-17): a
    /// later cell re-ran a `data Holder mode = Holder { probe :: ..., ... }`
    /// declaration an earlier cell had already installed, and using `probe`
    /// then finds it in both generations' selectors. The repair is reuse or
    /// rename, not a signature.
    const AMBIGUOUS_REDECLARED_FIELD: &str = "<cell>:24:9: error:\n    Ambiguous occurrence `probe'.\n    It could refer to\n       either the field `probe' of record `Tidepool.Session.Lib.G3.Holder',\n              imported from `Tidepool.Session.Lib.G3' at .../Tidepool.Session.Lib.G4.hs:51:1-30\n              (and originally defined in `Tidepool.Session.Lib.G2' at <cell>:4:71-75),\n           or the field `probe' of record `Tidepool.Session.Lib.G4.Holder',\n              defined at <cell>:4:71,\n           or `Tidepool.Session.Val.G25.probe',\n              imported from `Tidepool.Session.Val.G25' at .../Tidepool.Session.Lib.G4.hs:63:1-31.";

    #[test]
    fn a_redeclared_record_type_names_the_type_and_says_reuse_or_rename() {
        let advice = ambiguous_type_advice(AMBIGUOUS_REDECLARED_FIELD, "probe holder").unwrap();
        assert_eq!(
            advice,
            "`probe` is ambiguous because `Holder` was re-declared in this session \
             (Tidepool.Session.Lib.G3 and Tidepool.Session.Lib.G4 both define it); values \
             already in the session were built with the earlier `Holder`, so re-declaring a \
             type is refused here — reuse the earlier `Holder` declaration instead of \
             re-running it, or rename this one and its fields if you meant a distinct type"
        );
        // The compiler's own text survives: it names the occurrence, both
        // generations, and the line. The advice is added after it, not
        // substituted for it.
        let rendered = render_cell_compile_error(&cell_error(AMBIGUOUS_REDECLARED_FIELD), "probe holder");
        assert!(rendered.contains("Ambiguous occurrence"), "{rendered}");
        assert!(rendered.ends_with(&advice), "{rendered}");
    }

    /// The same diagnostic shape, but for a plain VALUE re-declared across two
    /// generations (no "of record"/"of class" framing anywhere in GHC's
    /// text) — e.g. re-running a helper function definition to refine it, the
    /// exact workbench workflow the session is meant to support. A value
    /// redeclaration is supposed to shadow silently (see `render_module`'s
    /// `hidden_prior`); GHC still reporting an ambiguity here means shadowing
    /// didn't apply for this turn. The advice must not call `symA` "a type" —
    /// that was the asymmetry bug: the old message always said "never
    /// re-declare a type that already exists", even for a plain function.
    const AMBIGUOUS_REDECLARED_VALUE: &str = "<cell>:8:1: error:\n    Ambiguous occurrence `symA'.\n    It could refer to\n       either `Tidepool.Session.Lib.G7.symA',\n              imported from `Tidepool.Session.Lib.G7' at <cell>:8:1\n              (and originally defined at <cell>:3:1),\n           or `Tidepool.Session.Lib.G8.symA',\n              defined at <cell>:8:1.";

    #[test]
    fn a_redeclared_value_names_it_as_a_value_not_a_type() {
        let advice = ambiguous_type_advice(AMBIGUOUS_REDECLARED_VALUE, "symA").unwrap();
        assert_eq!(
            advice,
            "`symA` is ambiguous because `symA` was declared again in this session \
             (Tidepool.Session.Lib.G7 and Tidepool.Session.Lib.G8 both define it); an ordinary \
             declaration is meant to shadow the earlier one automatically, so this is a \
             session-scoping gap, not something wrong with your code — reuse `symA` as already \
             declared, or give this one a different name to work around it for now"
        );
        assert!(
            !advice.contains("type"),
            "a plain value must never be called a type: {advice}"
        );
        let rendered = render_cell_compile_error(&cell_error(AMBIGUOUS_REDECLARED_VALUE), "symA");
        assert!(rendered.contains("Ambiguous occurrence"), "{rendered}");
        assert!(rendered.ends_with(&advice), "{rendered}");
    }

    /// The same-cell shape: a cell re-declares `sh` AND uses it from a bind
    /// statement in that same cell. The whole-cell preflight check names
    /// itself as the CANDIDATE next generation (G8, holding the cell's own
    /// fresh `sh`) while importing the CURRENT generation (G7, the earlier
    /// `sh`) unqualified — both visible at once. `same_cell_value_collisions`
    /// must name exactly `sh` as the retry target.
    const AMBIGUOUS_SAME_CELL_VALUE: &str = "<cell>:16:12-15: error:\n    Ambiguous occurrence `sh'.\n    It could refer to\n       either `Tidepool.Session.Lib.G7.sh',\n              imported from `Tidepool.Session.Lib.G7' at <cell>:3:1-2,\n           or `Tidepool.Session.Lib.G8.sh',\n              defined at <cell>:4:1.";

    #[test]
    fn same_cell_value_collisions_names_the_redeclared_bind_target() {
        assert_eq!(
            same_cell_value_collisions(
                AMBIGUOUS_SAME_CELL_VALUE,
                "Tidepool.Session.Lib.G7",
                "Tidepool.Session.Lib.G8",
            ),
            vec!["sh".to_string()]
        );
        // Swapping the module roles yields nothing — the collision is only
        // reported between the two modules actually named in the diagnostic.
        assert!(same_cell_value_collisions(
            AMBIGUOUS_SAME_CELL_VALUE,
            "Tidepool.Session.Lib.G3",
            "Tidepool.Session.Lib.G8",
        )
        .is_empty());
    }

    #[test]
    fn same_cell_value_collisions_ignores_a_genuine_type_redeclaration() {
        // The Holder/probe case is a TYPE collision (a field "of record") —
        // it must stay refused, never handed to the same-cell retry as a
        // value to hide.
        assert!(same_cell_value_collisions(
            AMBIGUOUS_REDECLARED_FIELD,
            "Tidepool.Session.Lib.G3",
            "Tidepool.Session.Lib.G4",
        )
        .is_empty());
    }

    /// Run 7's first friction: `unfold "run7/wave1"`. A fork group path is a
    /// pair, so unlike `GitRef` it cannot take a string literal, and the
    /// diagnostic alone leaves the reader to find `batch` by search.
    #[test]
    fn a_type_that_needs_a_constructor_names_it() {
        let literal = "<cell>:42:22: error:\n    No instance for `GHC.Internal.Data.String.IsString ForkGroupPath' arising from the literal \"run7/wave1\"";
        let advice = constructor_advice(literal).unwrap();
        assert!(advice.contains("batch \"campaign\" \"group\""), "{advice}");

        let mismatch = "<cell>:1:1: error:\n    Couldn't match expected type `WorktreeSpec' with actual type `WorktreeSeed'";
        assert!(
            constructor_advice(mismatch).unwrap().contains("fromRef"),
            "a seed where a spec was wanted should name the spec's constructors"
        );
        // The diagnostic survives: it says which types, which the advice does not.
        let rendered = render_cell_compile_error(&cell_error(mismatch), "createWorktree boundHead");
        assert!(rendered.contains("WorktreeSeed"), "{rendered}");
        assert!(rendered.ends_with(&constructor_advice(mismatch).unwrap()), "{rendered}");
    }

    #[test]
    fn a_type_that_takes_a_literal_is_left_to_its_instance() {
        // GitRef and BranchName have IsString, so a literal already works and
        // there is nothing to say.
        let git_ref = "<cell>:1:1: error:\n    Couldn't match expected type `GitRef' with actual type `[Char]'";
        assert_eq!(constructor_advice(git_ref), None);
    }

    /// The exact text a cell gets today for `Right handle <- createWorktree …`
    /// when the worktree does not exist. Three layers of engine detail and a
    /// location in a generated wrapper; the reader wrote neither.
    const DO_BLOCK_FAILURE: &str = "prepared execution failed: Haskell exception raised: Pattern match failure in 'do' block at /tmp/nix-shell.FG8yXG/.tmpdKgy0W/Expr.hs:78:1-10";

    #[test]
    fn a_failed_pattern_bind_names_the_bind_and_says_to_inspect_the_value() {
        let cell = "Right tree <- createWorktree (fromRef (GitRef \"shoal/dry8\") \"merge\")\ntree";
        assert_eq!(
            runtime_failure_advice(DO_BLOCK_FAILURE, cell).unwrap(),
            "`Right tree` on line 1 did not match, so the cell stopped there; \
             the value is not shown, so bind it plainly (`tree <- …`) to see what it was"
        );
    }

    #[test]
    fn several_refutable_binds_name_the_first_without_claiming_which_failed() {
        let cell = "x <- pure 1\nJust a <- pure Nothing\nRight b <- pure (Left 2)";
        let advice = runtime_failure_advice(DO_BLOCK_FAILURE, cell).unwrap();
        assert!(advice.starts_with("a pattern bind, first `Just a` on line 2,"), "{advice}");
    }

    /// A cell with no refutable bind at all still gets the recovery, because
    /// the failing bind may be inside a `where` or a helper this cell called.
    #[test]
    fn a_pattern_failure_with_nothing_to_point_at_still_says_what_to_do() {
        let advice = runtime_failure_advice(DO_BLOCK_FAILURE, "runEverything").unwrap();
        assert!(advice.contains("bind that value plainly"), "{advice}");
    }

    /// Every other runtime failure keeps its own words.
    #[test]
    fn an_unrelated_runtime_failure_is_left_alone() {
        assert_eq!(
            runtime_failure_advice("prepared execution failed: division by zero", "1 `div` 0"),
            None
        );
    }

    /// A tuple or plain binder cannot fail to match, so neither is offered as
    /// the culprit.
    #[test]
    fn only_constructor_patterns_count_as_refutable() {
        assert!(refutable_binds("(a, b) <- pure (1, 2)").is_empty());
        assert!(refutable_binds("value <- pure 1").is_empty());
        assert_eq!(refutable_binds("Just v <- pure Nothing").len(), 1);
    }

    /// A plain single-generation ambiguous occurrence (e.g. a field name that
    /// also happens to be an imported top-level value, with no second
    /// `Tidepool.Session.Lib.G<n>` candidate) is not a redeclaration and GHC's
    /// own text survives.
    #[test]
    fn an_ambiguous_occurrence_without_two_generations_is_left_alone() {
        let message = "<cell>:1:1: error:\n    Ambiguous occurrence `probe'.\n    It could refer to\n       either the field `probe' of record `Tidepool.Session.Lib.G3.Holder',\n              defined at <cell>:4:71,\n           or `Tidepool.Session.Val.G25.probe',\n              imported from `Tidepool.Session.Val.G25' at .../Tidepool.Session.Lib.G4.hs:63:1-31.";
        assert_eq!(ambiguous_type_advice(message, "probe holder"), None);
        let rendered = render_cell_compile_error(&cell_error(message), "probe holder");
        assert!(
            rendered.contains("Ambiguous occurrence"),
            "GHC's own text must survive: {rendered}"
        );
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
            "{{CELL_PRAGMAS}}\n",
            "module CellCheck where\n",
            "import Prelude\n",
            "{{CELL_IMPORTS}}\n",
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
                prepared: None,
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
                prepared: None,
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
            "{{CELL_PRAGMAS}}\n",
            "module CellCheck where\n",
            "import Prelude\n",
            "{{CELL_IMPORTS}}\n",
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
        assert!(
            checked
                .checked_source
                .contains("__tidepool_cell_pin_1_h = h"),
            "the checked source must capture the inferred binder: {}",
            checked.checked_source
        );
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
                prepared: None,
            },
            &pins,
        )
        .unwrap();
        let TurnResult::Bind { bound, .. } = staged else {
            panic!("same-cell nominal staged item was not a bind");
        };
        assert_eq!(bound[0].type_display, "Maybe G");
    }

    /// A local, minimal `Eff` so these two tests can exercise the REAL
    /// [`super::super::workbench::resident_cell_check_template`] — the exact
    /// `TidepoolCellExpression`/`TidepoolCellPure` overlap the bug lives in —
    /// without standing up the actual workbench effect row. The cell's own
    /// declarations carry it (via `{{CELL_DECLS}}`), so the preamble stays
    /// the ordinary shape every other whole-cell-check test in this module
    /// already uses.
    fn eff_cell_preamble() -> String {
        format!(
            "{}\nmodule CellCheck where\nimport Prelude\nimport Data.Text (Text)\n\
             default (Int, Double, Text)\n",
            crate::session::EVAL_PRAGMAS,
        )
    }
    const EFF_DECLS: &str = concat!(
        "data Eff (effects :: [*]) value = Eff value\n",
        "instance Functor (Eff effects) where { fmap f (Eff a) = Eff (f a) }\n",
        "instance Applicative (Eff effects) where { pure = Eff ; (Eff f) <*> (Eff a) = Eff (f a) }\n",
        "instance Monad (Eff effects) where { (Eff a) >>= f = f a }\n",
    );
    const EFF_ROW: &str = "'[]";

    /// The bug: a resident cell whose only unit is `pure (1 :: Int)` is
    /// exactly [`is_cell_pure_dispatch_ambiguity`]'s shape against the real
    /// check template, [`check_cell`] alone rejects it, and
    /// [`check_cell_preferring_effectful`] admits it — with the author's own
    /// text and span preserved, not the scratch pin — by retrying with the
    /// final expression pinned into the effect row.
    #[test]
    fn a_final_pure_cell_is_accepted_as_effectful() {
        let Some(extract) = std::env::var_os("TIDEPOOL_CELL_TEST_EXTRACT") else {
            return;
        };
        let _daemon = TestEnvGuard::unset("TIDEPOOL_EXTRACT_DAEMON_SOCKET");
        let _extract = TestEnvGuard::set("TIDEPOOL_EXTRACT", extract);
        let root = tempfile::tempdir().unwrap();
        let prelude = tidepool_testing::eval_harness::prelude_path();
        let effects = tidepool_testing::eval_harness::effects_include();
        let include = [prelude.as_path(), effects[0].as_path(), effects[1].as_path()];
        let template = super::super::workbench::resident_cell_check_template(
            &eff_cell_preamble(),
            EFF_ROW,
            "",
        );
        let cell = format!("{EFF_DECLS}pure (1 :: Int)\n");

        // Without the fix: the whole-cell preflight alone rejects this cell.
        let bare_failure = check_cell(CellCheckRequest {
            cell_text: &cell,
            template: &template,
            include: &include,
            session_root: root.path(),
            inject_modules: &[],
        })
        .expect_err("an unpinned `pure` final expression is ambiguous against the real template");
        assert!(
            is_cell_pure_dispatch_ambiguity(&crate::classify_compile(&bare_failure.error).message),
            "the reproduced failure must be exactly the shape the fix targets: {}",
            crate::classify_compile(&bare_failure.error).message
        );

        // With the fix: the cell is admitted, and the final item keeps the
        // author's own source/span rather than the scratch pin.
        let checked = check_cell_preferring_effectful(CellCheckRequest {
            cell_text: &cell,
            template: &template,
            include: &include,
            session_root: root.path(),
            inject_modules: &[],
        })
        .expect("a final `pure <expr>` cell must be accepted as effectful");
        let final_item = checked.items.last().expect("cell has at least one item");
        assert_eq!(final_item.verdict.kind, TurnKind::Expr);
        assert_eq!(final_item.source, "pure (1 :: Int)\n");
    }

    /// A genuinely pure final expression (no `Applicative`/`Monad` ambiguity
    /// at all — `1 + 1 :: Int` is not `Eff` anything) takes exactly the same
    /// path it always has: accepted on the first [`check_cell`] attempt, no
    /// retry involved.
    #[test]
    fn a_genuinely_pure_final_expression_still_takes_the_pure_path() {
        let Some(extract) = std::env::var_os("TIDEPOOL_CELL_TEST_EXTRACT") else {
            return;
        };
        let _daemon = TestEnvGuard::unset("TIDEPOOL_EXTRACT_DAEMON_SOCKET");
        let _extract = TestEnvGuard::set("TIDEPOOL_EXTRACT", extract);
        let root = tempfile::tempdir().unwrap();
        let prelude = tidepool_testing::eval_harness::prelude_path();
        let effects = tidepool_testing::eval_harness::effects_include();
        let include = [prelude.as_path(), effects[0].as_path(), effects[1].as_path()];
        let template = super::super::workbench::resident_cell_check_template(
            &eff_cell_preamble(),
            EFF_ROW,
            "",
        );
        let cell = format!("{EFF_DECLS}1 + 1 :: Int\n");

        let checked = check_cell(CellCheckRequest {
            cell_text: &cell,
            template: &template,
            include: &include,
            session_root: root.path(),
            inject_modules: &[],
        })
        .expect("a genuinely pure final expression must still be accepted outright");
        let final_item = checked.items.last().expect("cell has at least one item");
        assert_eq!(final_item.verdict.kind, TurnKind::Expr);
        assert_eq!(final_item.source, "1 + 1 :: Int\n");

        // check_cell_preferring_effectful must behave identically — no retry
        // is ever attempted for a cell that already succeeds outright.
        let via_wrapper = check_cell_preferring_effectful(CellCheckRequest {
            cell_text: &cell,
            template: &template,
            include: &include,
            session_root: root.path(),
            inject_modules: &[],
        })
        .expect("the wrapper must not reject what check_cell already accepts");
        assert_eq!(
            via_wrapper.items.last().unwrap().source,
            final_item.source
        );
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

    /// The exact two-line shape a model writes for a `let` binding with its
    /// signature on its own line (dogfood run, 2026-09-17): explicit braces
    /// disable layout, so without a hand-inserted `;` between the signature
    /// and its equation GHC reads the continuation line as more of the
    /// signature's type, not a second item in the group.
    #[test]
    fn render_template_turn_stmt_splits_signature_and_equation_on_separate_lines() {
        let source = "{{TURN_STMT}}pure ({{BINDERS}})\n";
        let turn_text = "let boxDefinition :: ActorSpec Box BoxEffects\n    boxDefinition = R.definition \"box\" (Actor.Selected knownEffects) Box { field = 1 }";
        let out = render_template(source, turn_text, &["boxDefinition".to_string()]);
        assert_eq!(
            out,
            "let { boxDefinition :: ActorSpec Box BoxEffects\n    ;boxDefinition = R.definition \"box\" (Actor.Selected knownEffects) Box { field = 1 }\n }\npure (boxDefinition)\n"
        );
    }

    /// A continuation line indented past the group's reference column (a
    /// multi-line expression, not a new item) gets no inserted `;`.
    #[test]
    fn render_template_turn_stmt_leaves_deeper_continuation_lines_alone() {
        let source = "{{TURN_STMT}}pure ({{BINDERS}})\n";
        let turn_text = "let total = 1\n             + 2";
        let out = render_template(source, turn_text, &["total".to_string()]);
        assert_eq!(out, "let { total = 1\n             + 2\n }\npure (total)\n");
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
            prepared: None,
        };
        let err = run_turn(req).unwrap_err();
        assert!(
            matches!(err.error, CompileError::ExtractFailed(_)),
            "expected a clean ExtractFailed, got {err:?}"
        );
    }

    /// A prepared turn is one compile that yields both halves: the Core the
    /// session runs today, and the turn target's prepared-STG program.
    #[test]
    fn prepared_turn_writes_its_program_beside_the_core_artifacts() {
        tidepool_testing::eval_harness::require_extract();
        let session_root = TempDir::new().unwrap();
        let templates = [TurnTemplate {
            kind: TemplateSelector::Expr,
            // A hand-written template still defines the settled scaffold the
            // prepared route projects; here the turn is pure, so it IS the value.
            source:
                "module Expr where\n__result :: Int\n__result = {{TURN}}\n__prepared = __result\n"
                    .to_string(),
        }];
        let request = |prepared| TurnRequest {
            turn_text: "41 + 1",
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
            prepared,
        };

        let TurnResult::Expr { compiled, .. } = run_turn(request(None)).unwrap() else {
            panic!("expr verdict produced another variant");
        };
        assert!(
            compiled.prepared.is_none(),
            "an ordinary turn compiles no prepared program"
        );

        let TurnResult::Expr { compiled, .. } =
            run_turn(request(Some(PreparedTurn { retained: &[] }))).unwrap()
        else {
            panic!("expr verdict produced another variant");
        };
        assert!(
            compiled.prepared.is_some(),
            "a prepared turn carries its own program"
        );
        // The Core half is unchanged by asking for the prepared half.
        assert!(!compiled.table.is_empty());
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
            prepared: None,
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
                CborValue::Array(vec![
                    CborValue::Array(vec![CborValue::Array(vec![]), CborValue::Array(vec![])]),
                    CborValue::Text("sq x = x * x".into()),
                ]),
            ]),
        ]);
        match decode_turn_out(&build_cbor(&v)).unwrap() {
            DecodedTurnOut::Decl {
                binders,
                items,
                source,
            } => {
                assert_eq!(binders, vec!["sq".to_string()]);
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].head_name(), "sq");
                assert_eq!(source.body, "sq x = x * x");
                assert!(source.prologue.imports.is_empty());
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
                prepared: None,
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
