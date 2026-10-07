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

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ciborium::value::Value as CborValue;
#[cfg(test)]
use tempfile::TempDir;
use tidepool_extract_cmd::{ExtractCmd, SpawnError};
use tidepool_toolchain::artifacts::{
    compiler_scratch_directory, decode_turn_nominal_heads as decode_nominal_heads,
    decode_turn_yield_sites as decode_asks, seal_turn_outputs, CompilerDiagnosticCapture,
    ModuleCandidateOffer,
};
use tidepool_toolchain::certified_products::{PendingCertifiedGroup, PendingImportOwner};
use tidepool_toolchain::checked_cell::{
    CheckedCellSpecification, ExactCheckedCell, ExactCheckedItem, ExactCompiledItem,
};
use tidepool_toolchain::extract_module_name;
use tidepool_toolchain::recovery_artifacts::CertifiedRecoveryProduct;

use tidepool_repr::execution_schema::{
    parse_program, DecodeLimits, PreparedProgram, SymbolIdentity,
};
use tidepool_repr::serial::{read_metadata, MetaWarnings};
use tidepool_repr::DataConTable;

use crate::{timing, CompileError, NominalHead, YieldSite};

use super::render::ExportItem;

#[cfg(test)]
#[path = "turn_scaling_tests.rs"]
pub(in crate::session) mod scaling_tests;

/// Strict-force tier of a bound value (mirrors the extract's `BoundBinder.tier`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueTier {
    /// First-order data — `deep_force`d to NF then tenured.
    ForceData,
    /// A closure/PAP — tenured as-is (not forced).
    RetainOpaque,
}

/// Closed compiler-issued authority for a host-built resident value. The
/// extractor classifies this from the exact root TyCon and its resolved unit;
/// it is never inferred from rendered type text or a constructor name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostBindingAuthority {
    JsonValue,
    Text,
    CommandJob,
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
    /// Exact nominal root of the persisted binding type, when its outer type
    /// is a type constructor. This is compiler-issued structural evidence,
    /// never parsed from `type_display` or inferred from nested type heads.
    pub root_head: Option<NominalHead>,
    /// The authenticated shipped type surface this binder belongs to, when it
    /// is one of the closed set the resident host can construct.
    pub host_authority: Option<HostBindingAuthority>,
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
    /// The grouped declaration item contains only the cell's leading
    /// pragmas/imports, with no authored declaration body.
    pub prologue_only: bool,
}

/// GHC-owned execution choice for one expression in a checked cell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckedExpressionPlan {
    pub key: String,
    pub lift: ExpressionLift,
    /// GHC-rendered full sigma type of the submitted expression. For an
    /// effectful expression this includes its exact `Eff` row.
    pub type_display: String,
    pub heads: Vec<NominalHead>,
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
    /// GHC-rendered type for presentation; native signatures own annotations.
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
    /// Plans are valid only for this exact submitted source and compile-view
    /// value generation.
    pub checked_cell_text: String,
    pub compile_generation: u64,
    pub compile_view_evidence: String,
    pub expression_plans: Vec<CheckedExpressionPlan>,
    /// GHC warnings from the final whole-cell check, in authored coordinates.
    pub warnings: Vec<crate::diag::ExtractDiag>,
    authority: Option<Arc<ExactCheckedCell>>,
    admission: Option<Arc<super::RuntimeCellAdmission>>,
}

/// Editable diagnostic observations carry no checked execution authority.
#[derive(Clone, Debug, Default)]
pub struct CellCheckObservations {
    pub prologue: SourcePrologue,
    pub items: Vec<CellAnalysisItem>,
    pub pins: Vec<CheckedBinderPin>,
    pub checked_source: String,
    pub checked_cell_text: String,
    pub compile_generation: u64,
    pub compile_view_evidence: String,
    pub expression_plans: Vec<CheckedExpressionPlan>,
    pub warnings: Vec<crate::diag::ExtractDiag>,
}

impl From<CellCheckObservations> for CellCheck {
    fn from(observation: CellCheckObservations) -> Self {
        Self {
            prologue: observation.prologue,
            items: observation.items,
            pins: observation.pins,
            checked_source: observation.checked_source,
            checked_cell_text: observation.checked_cell_text,
            compile_generation: observation.compile_generation,
            compile_view_evidence: observation.compile_view_evidence,
            expression_plans: observation.expression_plans,
            warnings: observation.warnings,
            authority: None,
            admission: None,
        }
    }
}

impl CellCheck {
    pub fn checked_item(&self, item_index: usize) -> Result<ExactCheckedItem, CompileError> {
        let authority = self.authority.as_ref().ok_or_else(|| {
            CompileError::ExtractFailed(
                "cell observations have no runtime-admitted compiler authority".into(),
            )
        })?;
        if self.checked_source != authority.checked_source()
            || self.items.len() != authority.item_count()
        {
            return Err(CompileError::ExtractFailed(
                "checked module observation was edited".into(),
            ));
        }
        let item = authority.item(item_index)?;
        let observation = self.items.get(item_index).ok_or_else(|| {
            CompileError::ExtractFailed("checked item observation is absent".into())
        })?;
        let kind = match observation.verdict.kind {
            TurnKind::Decl => tidepool_toolchain::checked_cell::CheckedItemKind::Declaration,
            TurnKind::Bind => tidepool_toolchain::checked_cell::CheckedItemKind::Bind,
            TurnKind::Expr => tidepool_toolchain::checked_cell::CheckedItemKind::Expression,
        };
        let pins = self.pins.iter().map(encode_checked_pin).collect::<Vec<_>>();
        let expressions = self
            .expression_plans
            .iter()
            .map(encode_checked_expression)
            .collect::<Vec<_>>();
        item.validate_observations(
            &observation.source,
            kind,
            &observation.verdict.binders,
            &pins,
            &expressions,
        )?;
        Ok(item)
    }

    pub fn admission(&self) -> Option<&Arc<super::RuntimeCellAdmission>> {
        self.admission.as_ref()
    }
}

fn encode_nominal_heads(heads: &[NominalHead]) -> CborValue {
    CborValue::Array(
        heads
            .iter()
            .map(|head| {
                CborValue::Array(vec![
                    CborValue::Text(head.unit.clone()),
                    CborValue::Text(head.module.clone()),
                    CborValue::Text(head.name.clone()),
                ])
            })
            .collect(),
    )
}
fn encode_checked_pin(pin: &CheckedBinderPin) -> CborValue {
    CborValue::Array(vec![
        CborValue::Text(pin.key.clone()),
        CborValue::Text(pin.ty.clone()),
        encode_nominal_heads(&pin.heads),
    ])
}
fn encode_checked_expression(plan: &CheckedExpressionPlan) -> CborValue {
    CborValue::Array(vec![
        CborValue::Text(plan.key.clone()),
        CborValue::Text(
            match plan.lift {
                ExpressionLift::Effectful => "effectful",
                ExpressionLift::Pure => "pure",
            }
            .into(),
        ),
        CborValue::Text(plan.type_display.clone()),
        encode_nominal_heads(&plan.heads),
    ])
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
    /// Protected original products and selected lexical graph for this check.
    pub exact_context: Option<Arc<tidepool_toolchain::declaration_join::ExactCompileContext>>,
    pub cell_text: &'a str,
    pub template: &'a str,
    pub include: &'a [&'a Path],
    pub session_root: &'a Path,
    pub inject_modules: &'a [String],
    pub compile_generation: u64,
    pub compile_view_evidence: &'a str,
    /// This session's incarnation identity, forwarded as
    /// `--session-incarnation` when present. `None` for a caller with no
    /// session identity in hand (a one-shot check) — the worker keeps its
    /// unconditional `Tidepool.Session.*` memo eviction in that case,
    /// exactly as before this field existed.
    pub session_id: Option<tidepool_repr::SessionId>,
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
///   exactly one of the two. Declaration templates also carry
///   `{{CELL_IMPORTS}}` for the compiler-parsed prologue imports, before
///   declaration bodies enter the template.
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
/// wrapper, with a header import slot and a `{{TURN}}` declaration slot.
///
/// The pragma block is deliberately a SUBSET of the canonical eval dialect
/// ([`super::EVAL_PRAGMAS`]): this pass PARSES but never
/// typechecks or renames, so extensions that only affect type inference,
/// instance resolution, or scoping have no business here. That delta is
/// declared and enforced — `tidepool-mcp`'s `pragma_set_consistency` test
/// asserts an exact subset with the excluded set spelled out, so adding an
/// extension here that eval does not also carry fails loud rather than
/// silently parsing session-decl code in a different dialect than eval does.
pub const DECL_TEMPLATE_SOURCE: &str = "{-# LANGUAGE GADTs, OverloadedStrings, TypeOperators, DataKinds, KindSignatures, ScopedTypeVariables, BangPatterns, ViewPatterns, TupleSections, MultiWayIf, LambdaCase, RecordWildCards, NamedFieldPuns, DeriveFunctor, DeriveFoldable, DeriveTraversable, TypeApplications, QuasiQuotes, OverloadedLabels #-}\nmodule SessionDecls where\n{{CELL_IMPORTS}}\n{{TURN}}\n";

/// One `run_turn` request: the raw turn text, the wrapper templates it may
/// need, the session context, the bind generation, and an optional
/// caller-supplied verdict.
///
/// When `verdict` is `Some`, [`run_turn`] forwards it to the extract via
/// `--turn-verdict`, skipping the extract's own re-parse — this is the
/// batch-classify case, where a caller already holds a GHC-sourced verdict
/// for the whole block (from [`classify_block`]). GHC-sourced either way.
pub struct TurnRequest<'a> {
    /// Protected original products and selected lexical graph for this turn.
    pub exact_context: Option<Arc<tidepool_toolchain::declaration_join::ExactCompileContext>>,
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
    /// The binding a compiling template names as its target. `None`
    /// means the extract's scaffold-reserved default (`__result`), which is
    /// what a template authored for this path should use. Supply it only when
    /// a template's target binder is fixed by a builder shared with another
    /// path — `exomonad-harness`'s expression wrapper comes from
    /// `tidepool_mcp::template_haskell`, which the stateless eval server also
    /// uses and which names its target `result`. Forwarded as `--target`; the
    /// output file base is `result.cbor` either way.
    pub target: Option<&'a str>,
    /// Executable imports retained by this turn. Empty for declarations and
    /// for the first turn in a machine session.
    pub retained_imports: &'a [(SymbolIdentity, u64)],
    /// This session's incarnation identity, forwarded as
    /// `--session-incarnation` when present. `None` for a caller with no
    /// session identity in hand (a one-shot eval) — the worker keeps its
    /// unconditional `Tidepool.Session.*` memo eviction in that case,
    /// exactly as before this field existed.
    pub session_id: Option<tidepool_repr::SessionId>,
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

/// The generic apply entry every prepared turn admits beside
/// [`PREPARED_SCAFFOLD_TARGET`] and [`PREPARED_RESUME_TARGET`]
/// (`Tidepool.Session.preparedApplyEntryTargetName`
/// on the worker side): `__applyEntry f n = settle (f (I# n))`, the entry
/// `ResidentSession::run_rooted_entry`/`run_rooted_entry_borrowed`
/// (`tidepool/runtime/src/session/resident.rs`) enters to apply a rooted
/// `Int -> Eff effects a` closure to a bare unboxed argument — actor program start,
/// shutdown hooks, actor source, and
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
/// Haskell value to another — both retained and neither bridged (actor
/// mailboxes, where the handler and
/// its protocol-indexed request are both live Haskell values). Same
/// polymorphism and evidence-interning exclusion as
/// [`PREPARED_APPLY_ENTRY_TARGET`].
pub const PREPARED_APPLY_VALUE_TARGET: &str = "__applyValue";

/// The qualified alias every template imports `Tidepool.Internal.Resume` under.
const RESUME_ALIAS: &str = "TidepoolResume";

/// Qualified text alias used by the activation-preview scaffold.
const TEXT_ALIAS: &str = "TidepoolScaffoldText";

/// The qualified alias every template imports `GHC.Exts` under, so
/// [`PREPARED_APPLY_ENTRY_TARGET`] can box its unboxed `Int#` argument
/// through `I#` without depending on a template's own imports.
const SCAFFOLD_EXTS_ALIAS: &str = "TidepoolScaffoldExts";

/// Every executable template ends with its settled scaffold, resume entry,
/// and generic apply entries. These auxiliary bindings are unreachable from
/// `__result`, so they do not enlarge its prepared dependency closure. Built from
/// [`prepared_scaffold_binding_named`] (the settled line) and
/// [`prepared_resume_apply_binding`] (the shared resume/apply group)
/// at the fixed [`PREPARED_SCAFFOLD_TARGET`]/[`PREPARED_RESUME_TARGET`]/
/// [`PREPARED_APPLY_ENTRY_TARGET`]/[`PREPARED_APPLY_VALUE_TARGET`] names every
/// resident-turn template uses.
///
/// Public so a caller assembling its OWN template outside
/// [`assemble_bind_module`]/[`assemble_expression_module`] (a hand-rolled
/// fixture in a test, say) can still append the exact scaffold a prepared
/// compile requires, rather than hand-duplicating these binder names.
pub fn prepared_scaffold_binding(target: &str) -> String {
    let mut out = prepared_scaffold_binding_named(PREPARED_SCAFFOLD_TARGET, target);
    out.push_str(&prepared_resume_apply_binding());
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
/// Does NOT emit the fixed auxiliary entries — see
/// [`prepared_resume_apply_binding`]'s doc for why those stay
/// fixed-named and module-shared rather than following `scaffold_target`.
#[must_use]
pub fn prepared_scaffold_binding_named(scaffold_target: &str, target: &str) -> String {
    format!("{scaffold_target} = {RESUME_ALIAS}.settle {target}\n")
}

/// The resume and generic-apply entries a module needs beside its
/// settled scaffold line(s) — see [`prepared_scaffold_binding`]'s doc for
/// what each does. UNLIKE the settled line itself, these are fixed at
/// [`PREPARED_RESUME_TARGET`]/[`PREPARED_APPLY_ENTRY_TARGET`]/
/// [`PREPARED_APPLY_VALUE_TARGET`] no matter
/// how many targets a module settles: the runtime resolves a program's
/// resume/apply roots by looking these exact names up in the
/// program's own top-level bindings (`ProgramFacts::of` in
/// `tidepool/runtime/src/session/prepared.rs` scans for
/// `identity.occurrence == PREPARED_RESUME_TARGET`/
/// `PREPARED_APPLY_ENTRY_TARGET`/`PREPARED_APPLY_VALUE_TARGET`), and none of
/// these bodies take a target-specific argument (`resumeLifted` and the apply
/// roots' own `settle` are the same computation
/// regardless of which settled entry suspended) — so a module settling
/// several targets ([`with_settled_scaffolds`] in `exomonad-harness::engine`)
/// emits this ONCE for the whole module, not once per target the way
/// [`prepared_scaffold_binding_named`]'s settled line must be.
///
/// `__applyEntry`/`__applyValue` take no explicit signature:
/// each has explicit parameters, so the monomorphism restriction never
/// applies and GHC infers `n`'s type as `Int#` directly from its use as
/// `I#`'s argument.
#[must_use]
pub fn prepared_resume_apply_binding() -> String {
    format!(
        "{PREPARED_RESUME_TARGET} q x = {RESUME_ALIAS}.settle ({RESUME_ALIAS}.resumeLifted q x)\n\
         {PREPARED_APPLY_ENTRY_TARGET} f n = {RESUME_ALIAS}.settle (f ({SCAFFOLD_EXTS_ALIAS}.I# n))\n\
         {PREPARED_APPLY_VALUE_TARGET} f x = {RESUME_ALIAS}.settle (f x)\n"
    )
}

/// The bare import targets (no leading `import `, one per line)
/// [`prepared_scaffold_binding`]/[`prepared_resume_apply_binding`]'s aliases
/// need in scope. [`with_resume_import`] splices these in through
/// [`insert_preamble_imports`]'s own marker
/// ([`PREAMBLE_IMPORT_MARKER`], the production preamble's shape); a caller
/// assembling a differently-shaped preamble (a test fixture with its own
/// import-splicing marker) can still get the exact same alias names by
/// splicing this text in itself, rather than hand-duplicating the aliases.
#[must_use]
pub fn resume_import_targets() -> String {
    format!(
        "qualified Tidepool.Internal.Resume as {RESUME_ALIAS}\n\
         qualified Data.Text as {TEXT_ALIAS}\n\
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
/// correct habit inside a `do` block. The canonical cell template resolves
/// this shape through its named `Eff` default, so a reader sees this advice
/// only when another error prevents the whole cell from typechecking.
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
/// family [`ambiguous_type_advice`] otherwise handles. An ordinary ambiguous
/// binding (an unconstrained `Render a0`, say) does not carry an
/// `Applicative`/`Monad` constraint and does not match.
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
/// * `GHC.Types.ZonkAny` represents an ungeneralized metavariable. It cannot
///   become a valid source-level annotation, and no default list can name it.
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
    (unresolved_overlap || message.contains("ZonkAny")).then(|| AMBIGUOUS_TYPE_ADVICE.to_owned())
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
///   bound in the session's persistent binding store were built against the OLD shape, so
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
    let output = with_advice(rendered.text, |text| advice_for(text, cell_text));
    let output = with_stdlib_hint(output, &rendered.diagnostics);
    CompileRejection {
        output,
        diagnostics: rendered.diagnostics,
    }
}

/// Append a recognized-error hint, when one of the two narrow patterns below
/// matches one of the diagnostics GHC actually produced. Always ADDS to
/// GHC's own text, never replaces it — unlike [`with_advice`], which
/// sometimes substitutes plain language for a diagnostic that is an artifact
/// of cell wrapping. A no-op when the hint text is already present, so this
/// stays safe to call more than once on the same rendered output.
fn with_stdlib_hint(
    mut output: String,
    diagnostics: &[crate::diag::StructuredDiagnostic],
) -> String {
    let Some(hint) = stdlib_advice(diagnostics) else {
        return output;
    };
    if !output.contains(hint.as_ref()) {
        if !output.is_empty() {
            output.push_str("\n\n");
        }
        output.push_str(hint.as_ref());
    }
    output
}

/// Three harness-specific compile errors, chosen from dogfooding transcript
/// counts, that name the next valid operation. Matched narrowly against the
/// structured diagnostics GHC actually reported — never against arbitrary
/// rendered text, and never a general interpretation of GHC's output.
///
/// Deliberately does not attempt every GHC diagnostic shape: only the forms
/// observed repeatedly across dogfood/wave runs.
fn stdlib_advice(
    diagnostics: &[crate::diag::StructuredDiagnostic],
) -> Option<std::borrow::Cow<'static, str>> {
    diagnostics
        .iter()
        .find_map(|diagnostic| single_stdlib_advice(&diagnostic.message))
}

fn single_stdlib_advice(message: &str) -> Option<std::borrow::Cow<'static, str>> {
    if is_command_result_stream_mismatch(message) {
        return Some(std::borrow::Cow::Borrowed(COMMAND_RESULT_STREAM_ADVICE));
    }
    if is_string_text_mismatch(message) {
        return Some(std::borrow::Cow::Borrowed(STRING_TEXT_ADVICE));
    }
    if let Some(name) = text_alias_not_in_scope_name(message) {
        return Some(std::borrow::Cow::Owned(text_alias_advice(name)));
    }
    None
}

/// Text vocabulary re-exported unqualified by Tidepool.Prelude. The cell
/// preamble uses `T` for qualified operations; these names support diagnostics
/// for mistaken qualifiers such as `Text.pack`.
const TEXT_VOCAB_NAMES: &[&str] = &[
    "Text",
    "pack",
    "unpack",
    "unlines",
    "lines",
    "strip",
    "intercalate",
    "isInfixOf",
];

/// Whether `message` is a GHC not-in-scope diagnostic (`Variable not in
/// scope: X` for a value, `Not in scope: type constructor or class 'X'` for
/// the `Text` type) naming one of [`TEXT_VOCAB_NAMES`], returning the exact
/// name matched. Tokenized on non-alphanumerics (so a qualified reference
/// like `Text.pack` — GHC's shape for an unimported `Text` qualifier — is
/// found via its `Text` token) and matched as a whole token, so e.g.
/// `unpackSomething` does not false-fire.
fn text_alias_not_in_scope_name(message: &str) -> Option<&'static str> {
    let not_in_scope = message.contains("Variable not in scope")
        || message.contains("Not in scope: type constructor or class");
    if !not_in_scope {
        return None;
    }
    message
        .split(|c: char| !c.is_ascii_alphanumeric())
        .find_map(|token| {
            TEXT_VOCAB_NAMES
                .iter()
                .find(|name| **name == token)
                .copied()
        })
}

fn text_alias_advice(name: &str) -> String {
    format!("note: Data.Text is imported qualified as T; use T.{name} (Text is imported by name)")
}

const COMMAND_RESULT_STREAM_ADVICE: &str = "`Cmd.stdout`/`Cmd.stderr` read a retained \
    `Cmd.RunResult`, not the outcome-only `Cmd.CommandResult` a completion event delivers: \
    capture the job and use `Cmd.readStdout job` / `Cmd.readCommand job` instead, handling \
    the `Left` case.";

/// `Cmd.stdout`/`Cmd.stderr` (`stdout :: RunResult -> Either OutputIssue Text`,
/// `stderr :: RunResult -> Text`, `Tidepool.Command`) applied to a
/// `Cmd.CommandResult` — what a retained job's completion event
/// (`Cmd.completion job :: R.EventSource CommandResult`) actually delivers.
/// Narrow: both type names must appear in the same "Couldn't match" block,
/// and the block must name the function actually applied.
fn is_command_result_stream_mismatch(message: &str) -> bool {
    message.contains("Couldn't match")
        && message.contains("RunResult")
        && message.contains("CommandResult")
        && (names_quoted(message, "stdout") || names_quoted(message, "stderr"))
}

const STRING_TEXT_ADVICE: &str = "the stdlib is Text-first. A string literal is already \
    `Text`, so pass it as written; a `String` value, such as the result of `show`, needs \
    `T.pack`, and a `Text` going where `String` is wanted needs `T.unpack`.";

/// `[Char]`/`String` vs `Text` at an argument of a stdlib function. Narrow:
/// requires an actual "Couldn't match" block naming both `Text` and one of
/// GHC's two spellings for a string literal's inferred type.
fn is_string_text_mismatch(message: &str) -> bool {
    message.contains("Couldn't match")
        && message.contains("Text")
        && (message.contains("[Char]") || message.contains("String"))
}

/// Whether `message` quotes `name` as a whole identifier, in any of the
/// quote styles GHC's pretty-printer uses across configurations.
fn names_quoted(message: &str, name: &str) -> bool {
    message.contains(&format!("\u{2018}{name}\u{2019}"))
        || message.contains(&format!("`{name}'"))
        || message.contains(&format!("`{name}`"))
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
    /// This turn's DataCon metadata.
    pub table: DataConTable,
    /// Compile warnings (e.g. `has_io`).
    pub warnings: MetaWarnings,
    /// Typed suspension sites decoded from the `TurnOut` wire payload.
    pub asks: Vec<YieldSite>,
    /// The turn's prepared-STG program.
    pub prepared: Arc<PreparedProgram>,
    /// Worker-certified original source groups and the target's exact owner
    /// rows. Retained imports still require lexical resolution at admission.
    pub certification: Option<TurnCertification>,
}

#[derive(Clone, Debug)]
pub struct TurnCertification {
    pub(crate) artifact_view: tidepool_toolchain::artifact_inventory::ArtifactView,
    pub(crate) original_compile_input:
        Option<Arc<tidepool_toolchain::artifacts::SealedOriginalCompileInput>>,
    pub groups: Arc<[PendingCertifiedGroup]>,
    pub target_owners: Vec<PendingImportOwner>,
    pub package_interfaces:
        tidepool_toolchain::certified_products::CertifiedTargetPackageInterfaces,
    /// Exact owned compiler products for recovery publication after admission.
    pub recovery_products: Vec<CertifiedRecoveryProduct>,
    purpose: TurnPurpose,
}

/// Checked runtime output is ready only with its original prefix owner.
/// Compiler bundle identity remains independent of this execution purpose.
#[derive(Clone, Debug)]
pub(super) enum TurnPurpose {
    Ordinary,
    Execution {
        execution: Arc<ExactCompiledItem>,
        prefix: Arc<super::RuntimeCheckedPrefix>,
    },
    ActivationPreview(Arc<tidepool_toolchain::activation_preview::ExactCompiledActivationPreview>),
    /// Immutable constructor code from an original checked host item. This
    /// carries no runtime prefix, authored execution or fresh binding authority.
    HostPrototype(Arc<ExactCompiledItem>),
}

impl TurnPurpose {
    fn from_sealed(
        proof: Option<&tidepool_toolchain::artifacts::CheckedNativeProof>,
        admission: Option<&Arc<super::RuntimeCheckedItemAdmission>>,
    ) -> Result<Self, CompileError> {
        use tidepool_toolchain::artifacts::CheckedNativeProof;
        let Some(admission) = admission else {
            return match proof {
                None => Ok(Self::Ordinary),
                Some(CheckedNativeProof::Execution(_)) => Err(CompileError::ExtractFailed(
                    "checked execution lacks its runtime prefix owner".into(),
                )),
                Some(CheckedNativeProof::ActivationPreview(proof)) => {
                    Ok(Self::ActivationPreview(proof.clone()))
                }
            };
        };
        let Some(CheckedNativeProof::Execution(execution)) = proof else {
            return Err(CompileError::ExtractFailed(
                "checked item lacks a sealed execution recipe".into(),
            ));
        };
        Self::execution(execution.clone(), admission)
    }

    fn execution(
        execution: Arc<ExactCompiledItem>,
        admission: &Arc<super::RuntimeCheckedItemAdmission>,
    ) -> Result<Self, CompileError> {
        if execution.item() != admission.item()
            || execution.generation() != admission.generation().0
        {
            return Err(CompileError::ExtractFailed(
                "prepared item differs from its original reservation".into(),
            ));
        }
        execution.validate_runtime_admission(
            admission.digest(),
            admission.prefix().admission().digest(),
        )?;
        Ok(Self::Execution {
            execution,
            prefix: admission.prefix().clone(),
        })
    }
}

impl Default for TurnCertification {
    fn default() -> Self {
        Self {
            artifact_view: tidepool_toolchain::artifact_inventory::ArtifactInventory::default()
                .empty_view(),
            original_compile_input: None,
            groups: Arc::from([]),
            target_owners: Vec::new(),
            package_interfaces: Default::default(),
            recovery_products: Vec::new(),
            purpose: TurnPurpose::Ordinary,
        }
    }
}

impl TurnCertification {
    /// Carry an ordinary compiler target's complete certified import closure.
    pub(crate) fn from_artifacts(
        artifacts: &crate::CompiledArtifacts,
        target: &crate::TargetArtifact,
    ) -> Self {
        Self {
            artifact_view: artifacts.artifact_view.clone(),
            groups: artifacts.certified_groups.clone().into(),
            target_owners: target.pending_imports.clone(),
            package_interfaces: target.package_interfaces.clone(),
            recovery_products: artifacts.recovery_products.clone(),
            ..Default::default()
        }
    }

    pub(super) fn host_prototype(&self) -> Result<Self, CompileError> {
        let TurnPurpose::Execution { execution, prefix } = &self.purpose else {
            return Err(CompileError::ExtractFailed(
                "host prototype lacks original checked ownership".into(),
            ));
        };
        if prefix.admission().host_carrier().is_none() {
            return Err(CompileError::ExtractFailed(
                "authored execution cannot issue a host prototype".into(),
            ));
        }
        let mut retained = self.clone();
        retained.purpose = TurnPurpose::HostPrototype(execution.clone());
        Ok(retained)
    }
    pub(super) fn purpose(&self) -> &TurnPurpose {
        &self.purpose
    }
    pub fn checked_item(&self) -> Option<&ExactCheckedItem> {
        match &self.purpose {
            TurnPurpose::Execution { execution, .. } => Some(execution.item()),
            _ => None,
        }
    }
    pub fn checked_execution(&self) -> Option<&Arc<ExactCompiledItem>> {
        match &self.purpose {
            TurnPurpose::Execution { execution, .. } => Some(execution),
            _ => None,
        }
    }
    pub(crate) fn checked_activation_preview(
        &self,
    ) -> Option<&Arc<tidepool_toolchain::activation_preview::ExactCompiledActivationPreview>> {
        match &self.purpose {
            TurnPurpose::ActivationPreview(proof) => Some(proof),
            _ => None,
        }
    }
    pub fn checked_prefix(&self) -> Option<&Arc<super::RuntimeCheckedPrefix>> {
        match &self.purpose {
            TurnPurpose::Execution { prefix, .. } => Some(prefix),
            _ => None,
        }
    }

    pub(crate) fn validate_checked_table(&self, table: &DataConTable) -> Result<(), CompileError> {
        match &self.purpose {
            TurnPurpose::Ordinary => Ok(()),
            TurnPurpose::Execution { execution, .. } | TurnPurpose::HostPrototype(execution) => {
                execution.validate_table(table)
            }
            TurnPurpose::ActivationPreview(proof) => proof.validate_table(table),
        }
    }

    pub(crate) fn validate_checked_bind(
        &self,
        target: &PreparedProgram,
        generation: u64,
        bound: &[BoundBinder],
    ) -> Result<(), CompileError> {
        let execution = match &self.purpose {
            TurnPurpose::Ordinary => return Ok(()),
            TurnPurpose::ActivationPreview(_) | TurnPurpose::HostPrototype(_) => {
                return Err(CompileError::ExtractFailed(
                    "protected host output requires its owning runtime consumer".into(),
                ))
            }
            TurnPurpose::Execution { execution, .. } => execution,
        };
        if !execution.matches_target(target) || execution.generation() != generation {
            return Err(CompileError::ExtractFailed(
                "checked execution target or generation was edited".into(),
            ));
        }
        let rows = bound
            .iter()
            .map(encode_bound_binder_authority)
            .collect::<Vec<_>>();
        execution.validate_bound_binders(&rows)
    }
}

pub(super) fn encode_bound_binder_authority(binder: &BoundBinder) -> CborValue {
    let text = |value: &str| CborValue::Text(value.into());
    CborValue::Array(vec![
        text(&binder.name),
        CborValue::Integer(binder.var_id.into()),
        text(&binder.module),
        text(match binder.tier {
            ValueTier::ForceData => "ForceData",
            ValueTier::RetainOpaque => "RetainOpaque",
        }),
        text(&binder.type_display),
        binder
            .root_head
            .as_ref()
            .map(|head| {
                CborValue::Array(vec![text(&head.unit), text(&head.module), text(&head.name)])
            })
            .unwrap_or(CborValue::Null),
        binder
            .host_authority
            .map(|authority| {
                text(match authority {
                    HostBindingAuthority::JsonValue => "JsonValue",
                    HostBindingAuthority::Text => "Text",
                    HostBindingAuthority::CommandJob => "CommandJob",
                })
            })
            .unwrap_or(CborValue::Null),
    ])
}

impl CompiledTurn {
    /// Reconstruct a fresh runtime installation from a complete original entry.
    /// Its original custody is checked by the toolchain loader; mutable runtime
    /// state is created only when this turn is installed in a session.
    pub fn from_production_entry(
        entry: &tidepool_toolchain::artifacts::ProductionEntryOutput,
    ) -> Result<Self, CompileError> {
        compiled_native_output(
            entry.table(),
            entry.warnings(),
            entry.yield_sites().to_vec(),
            entry.target_owned(),
            Some(entry.products()),
            None,
        )
    }

    /// Completed-original custody, independent of new source replay eligibility.
    pub fn original_compile_input(
        &self,
    ) -> Option<&Arc<tidepool_toolchain::artifacts::SealedOriginalCompileInput>> {
        self.certification.as_ref()?.original_compile_input.as_ref()
    }

    /// Exact original artifact identities retained by this compiler output.
    pub fn source_artifacts(
        &self,
    ) -> Vec<tidepool_toolchain::artifact_inventory::ArtifactDescriptor> {
        self.certification
            .as_ref()
            .map_or_else(Vec::new, |certification| {
                certification.artifact_view.descriptors()
            })
    }

    /// Borrow immutable inputs when the caller will reuse this artifact.
    #[must_use]
    pub fn code(&self) -> TurnCode<'_> {
        TurnCode {
            table: std::borrow::Cow::Borrowed(&self.table),
            sites: std::borrow::Cow::Borrowed(&self.asks),
            prepared: std::borrow::Cow::Borrowed(self.prepared.as_ref()),
            certification: std::borrow::Cow::Borrowed(&self.certification),
        }
    }

    /// Transfer a single-use turn into the machine without copying its graph.
    #[must_use]
    pub fn into_code(self) -> TurnCode<'static> {
        TurnCode {
            table: std::borrow::Cow::Owned(self.table),
            sites: std::borrow::Cow::Owned(self.asks),
            prepared: std::borrow::Cow::Owned(Arc::unwrap_or_clone(self.prepared)),
            certification: std::borrow::Cow::Owned(self.certification),
        }
    }
}

/// Immutable inputs for installation. Owned inputs move into the machine;
/// borrowed inputs remain reusable and are copied only at installation.
#[derive(Clone)]
pub struct TurnCode<'a> {
    pub table: std::borrow::Cow<'a, DataConTable>,
    pub sites: std::borrow::Cow<'a, [YieldSite]>,
    pub prepared: std::borrow::Cow<'a, PreparedProgram>,
    pub certification: std::borrow::Cow<'a, Option<TurnCertification>>,
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

/// Versioned import insertion point in a compiler-owned preamble.
pub const PREAMBLE_IMPORT_MARKER: &str = "-- tidepool-preamble-imports-v1\n";
/// The compiler qualifies these primitive default identities before inserting
/// authored declarations, preserving the independent import insertion marker.
pub const PREAMBLE_DEFAULT_DECL: &str = concat!(
    "-- tidepool-preamble-imports-v1\n",
    "default (Int, Double, Text)\n"
);

/// Insert `import <m>` lines into `preamble` immediately before its
/// [`PREAMBLE_IMPORT_MARKER`] line — the same injection point
/// `template_haskell` uses. A no-op (returns `preamble` unchanged) when
/// `imports` is blank. The ONE import-insertion mechanism a session-turn
/// module builder needs — `tidepool-repl`'s and `exomonad-harness`'s own turn
/// wrappers call this rather than reimplementing the same marker search.
pub fn insert_preamble_imports(preamble: &str, imports: &str) -> String {
    if imports.trim().is_empty() {
        return preamble.to_string();
    }
    let insert_point = preamble
        .find(PREAMBLE_IMPORT_MARKER)
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
/// ` ; pure <tail> }` closer.
///
/// This is the ONE mechanism behind every BIND/BINDDISCARD/MULTIBIND session
/// wrapper in both `tidepool-repl` (`wrap_bind_source`/
/// `wrap_bind_discard_source`/`wrap_multi_bind_source`) and `exomonad-harness`
/// (`template_session_bind`/`session_bind_template`) — those stay as each
/// crate's own thin, policy-only callers (what `extra`/`tail`
/// to pass), not a second copy of this assembly.
pub fn assemble_bind_module(
    preamble_with_imports: &str,
    extra: &str,
    target: &str,
    effect_stack: &str,
    stmt: &str,
    tail: &str,
) -> String {
    let mut out = with_resume_import(preamble_with_imports);
    out.push_str("-- [user]\n");
    out.push_str(extra);
    out.push_str(&format!("{target} = do {{\n"));
    out.push_str(stmt);
    // Pin only the effect row. Let GHC infer the result type so generated
    // workbench scaffolding cannot manufacture a partial-signature warning.
    out.push_str(&format!(
        " ; _ <- (pure () :: Eff {effect_stack} ())\n ; pure {tail}\n }}\n"
    ));
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
/// text byte-for-byte inside an explicit-layout binding. Prepared whole-cell
/// execution supplies the single lift selected by its checked expression
/// plan. Direct legacy callers may still supply ordered candidates.
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
        None,
    )
}

#[derive(Clone, Copy)]
enum ExpressionResult {
    Raw,
    OpaqueToken,
    Observation,
}

fn assemble_expression_module_with_result(
    preamble_with_imports: &str,
    target: &str,
    effect_stack: &str,
    expression: &str,
    lift: ExpressionLift,
    result: ExpressionResult,
    checked_expression_type: Option<&str>,
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
        out.push_str("__workbenchValue = __tidepoolInEffectRow $ let {\n");
    }
    if matches!(lift, ExpressionLift::Pure) {
        out.push_str("__workbenchValue = let {\n");
    }
    if let Some(ty) = checked_expression_type {
        out.push_str(&format!(" __value :: ({ty});\n"));
    }
    match lift {
        ExpressionLift::Effectful => out.push_str(" __value =\n"),
        ExpressionLift::Pure => out.push_str(" __value = __tidepoolPureWorkbenchValue $ "),
    }
    out.push_str(expression);
    if !expression.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(" } in __value\n");
    let body = match (lift, result) {
        (ExpressionLift::Effectful, ExpressionResult::Raw) => "__workbenchValue",
        (ExpressionLift::Pure, ExpressionResult::Raw) => "pure __workbenchValue",
        (ExpressionLift::Effectful, ExpressionResult::OpaqueToken) => {
            "do { _ <- __workbenchValue ; pure (T.pack \"<opaque value>\") }"
        }
        (ExpressionLift::Pure, ExpressionResult::OpaqueToken) => {
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
    checked_expression_type: Option<&str>,
) -> String {
    assemble_expression_module_with_result(
        preamble,
        target,
        effect_stack,
        expression,
        lift,
        ExpressionResult::Observation,
        checked_expression_type,
    )
}

/// Assemble an expression result without requiring a rendering instance.
/// Effectful values still run exactly once; pure values are evaluated to weak
/// head normal form. Only the fixed opaque token crosses into Rust, so
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
        ExpressionResult::OpaqueToken,
        None,
    )
}

/// Place `turn_text` as a `do`-block statement — the `{{TURN_STMT}}`
/// placement mode. Mirrors `tidepool-repl`'s and `exomonad-harness`'s own
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

/// Admit the configured deployment before any compiler offer, candidate read,
/// build-product selection, or worker body execution can use this endpoint.
fn bind_extract_cmd(
    command: &ExtractCmd,
) -> Result<tidepool_toolchain::toolchain::AdmittedCompilerEndpoint, CompileError> {
    let endpoint = command.bind().map_err(map_notfound)?;
    tidepool_toolchain::toolchain::AdmittedCompilerEndpoint::from_bound(endpoint)
        .map_err(|error| CompileError::ExtractFailed(error.to_string()))
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
/// This lower-level check returns diagnostic observations. Admitted execution
/// uses checked item recipes or the complete cell program.
#[tracing::instrument(name = "cell_check", level = "info", skip_all, fields(cell_bytes = req.cell_text.len()))]
pub fn check_cell(req: CellCheckRequest<'_>) -> Result<CellCheck, CellCheckFailure> {
    check_cell_impl(req, None, None)
}

/// Check against the runtime's retained view and interface bytes. The compiler
/// offer alone may turn the resulting observations into execution authority.
pub fn check_cell_admitted(
    req: CellCheckRequest<'_>,
    admission: Arc<super::RuntimeCellAdmission>,
    templates: &[TurnTemplate],
) -> Result<CellCheck, CellCheckFailure> {
    validate_cell_admitted_request(&req, &admission)?;
    if admission.plan_reservation().is_some() {
        return Err(CompileError::ExtractFailed(
            "planned cells require complete program compilation".into(),
        )
        .into());
    }
    if admission.reserved_generations().len() > 1 {
        return Err(CompileError::ExtractFailed(
            "checked cells support one initial reserved declaration group".into(),
        )
        .into());
    }
    check_cell_impl(req, Some(admission), Some(templates))
}

/// Result of preparing the original-type display after the live binding commits.
pub enum ActivationPreviewCompilation {
    Ready(super::CompiledActivationPreview),
    OriginalDisplayEvidenceUnavailable,
}

/// Compile a pure recipe against the mounted original input's exact interface.
pub fn compile_activation_preview(
    admission: Arc<super::RuntimeActivationPreviewAdmission>,
    template_source: &str,
    budget: u64,
    includes: &[PathBuf],
) -> Result<ActivationPreviewCompilation, TurnFailure> {
    use tidepool_toolchain::activation_preview::{
        ActivationPreviewSelection, ActivationPreviewSpecification,
    };
    let view = admission.view();
    let context = admission.exact_context().clone();
    let temp = compiler_scratch_directory()?;
    let input_path = temp.path().join("ActivationPreviewTemplate.hs");
    std::fs::write(&input_path, template_source)?;
    let mut cmd = extract_cmd()?;
    cmd.input(&input_path)
        .turn()
        .activation_preview()
        .turn_out(temp.path().join("turn.cbor"))
        .output_dir(temp.path())
        .includes(includes)
        .session_root(view.session_root())
        .session_incarnation(view.session().0.to_string())
        .bind_gen(admission.generation().0);
    let endpoint = bind_extract_cmd(&cmd)?;
    let selection = ModuleCandidateOffer::select_activation_preview(
        &endpoint,
        includes,
        temp.path(),
        context,
        admission.input_interface().clone(),
        ActivationPreviewSpecification {
            admission_digest: admission.digest(),
            generation: admission.generation().0,
            budget,
            template_source: template_source.into(),
        },
    )?;
    let ActivationPreviewSelection::Ready(offer) = selection else {
        return Ok(ActivationPreviewCompilation::OriginalDisplayEvidenceUnavailable);
    };
    if let Some(root) = offer.checked_value_root() {
        cmd.session_root(root);
    }
    offer.apply_to(&mut cmd)?;
    crate::paths::apply_admitted_build_products_dir(&mut cmd, &endpoint);
    let diagnostics = CompilerDiagnosticCapture::start(temp.path(), &cmd);
    let run = endpoint
        .execute(&cmd)
        .map_err(|error| offer.retain_execution_failure(temp.path(), &cmd, map_notfound(error)))?;
    diagnostics.completed(temp.path(), &cmd, run.success(), &run.output.stderr);
    timing::log_interface_counts(&run.output.stderr);
    forward_extract_timing(&String::from_utf8_lossy(&run.output.stderr), "extract");
    crate::diag::decode_extract_result(
        run.output.status.success(),
        &run.output.stdout,
        &run.output.stderr,
    )
    .map_err(|error| offer.retain_failure(temp.path(), &cmd, &run.output.stderr, error))?;
    if offer
        .activation_preview_unavailable(temp.path())
        .map_err(|error| offer.retain_failure(temp.path(), &cmd, &run.output.stderr, error))?
    {
        return Ok(ActivationPreviewCompilation::OriginalDisplayEvidenceUnavailable);
    }
    let result = decode_turn_output_dir(temp.path(), &offer, None)
        .map_err(|error| offer.retain_failure(temp.path(), &cmd, &run.output.stderr, error))?;
    let TurnResult::Expr { compiled, .. } = result else {
        return Err(CompileError::ExtractFailed(
            "activation preview produced a binding or declaration".into(),
        )
        .into());
    };
    let proof = compiled
        .certification
        .as_ref()
        .and_then(TurnCertification::checked_activation_preview)
        .ok_or_else(|| {
            CompileError::ExtractFailed("activation preview lacks its sealed native proof".into())
        })?
        .clone();
    if proof.admission_digest() != admission.digest()
        || proof.generation() != admission.generation().0
        || !Arc::ptr_eq(proof.input_interface(), admission.input_interface())
        || !proof.matches_target(&compiled.prepared)
    {
        return Err(CompileError::ExtractFailed(
            "activation preview differs from its mounted input admission".into(),
        )
        .into());
    }
    proof.validate_table(&compiled.table)?;
    proof.validate_yield_sites(&compiled.asks)?;
    Ok(ActivationPreviewCompilation::Ready(
        super::CompiledActivationPreview {
            admission,
            compiled,
            proof,
        },
    ))
}

fn validate_cell_admitted_request(
    req: &CellCheckRequest<'_>,
    admission: &super::RuntimeCellAdmission,
) -> Result<(), CellCheckFailure> {
    let view = admission.view();
    if admission.private_execution().is_none()
        && !admission.is_native_setup()
        && admission.host_carrier().is_none()
    {
        return Err(CompileError::ExtractFailed(
            "checked execution requires its owning private or native setup admission".into(),
        )
        .into());
    }
    if req.include.len() != admission.include_paths().len()
        || req
            .include
            .iter()
            .zip(admission.include_paths())
            .any(|(requested, admitted)| requested.as_os_str() != admitted.as_os_str())
    {
        return Err(CompileError::InputRejected(vec![crate::diag::ExtractDiag {
            span: None,
            severity: crate::diag::DiagnosticSeverity::Error,
            message: "checked request source search inputs differ from its runtime admission"
                .into(),
        }])
        .into());
    }
    if req.session_id != Some(view.session())
        || req.session_root != view.session_root()
        || req.compile_generation != view.next_value_generation().0
        || req.inject_modules != view.injected_module_names()
        || req.exact_context != view.exact_compile_context()
    {
        return Err(CompileError::ExtractFailed(
            "checked-cell request differs from the protected runtime admission".into(),
        )
        .into());
    }
    Ok(())
}

/// Prepare every authored item before the caller can execute the
/// first native effect. The compiler owns and seals the complete immutable
/// program, including original declaration and Val interface identities.
pub fn compile_cell_program_admitted(
    req: CellCheckRequest<'_>,
    admission: Arc<super::RuntimeCellAdmission>,
    templates: &[TurnTemplate],
) -> Result<
    (
        CellCheck,
        Arc<tidepool_toolchain::checked_cell::CellProgram>,
    ),
    CellCheckFailure,
> {
    compile_cell_program_admitted_inner(
        req,
        admission,
        templates,
        #[cfg(test)]
        CellProgramAudit::None,
    )
}

#[cfg(test)]
fn compile_cell_program_admitted_receipt_controls(
    req: CellCheckRequest<'_>,
    admission: Arc<super::RuntimeCellAdmission>,
    templates: &[TurnTemplate],
) -> Result<
    (
        CellCheck,
        Arc<tidepool_toolchain::checked_cell::CellProgram>,
    ),
    CellCheckFailure,
> {
    compile_cell_program_admitted_inner(req, admission, templates, CellProgramAudit::Receipts)
}

#[cfg(test)]
#[derive(Clone, Copy)]
enum CellProgramAudit {
    None,
    Receipts,
    WorkCounts(Option<usize>),
}

#[cfg(test)]
fn compile_cell_program_admitted_work_controls(
    req: CellCheckRequest<'_>,
    admission: Arc<super::RuntimeCellAdmission>,
    templates: &[TurnTemplate],
    expected_items: Option<usize>,
) -> Result<
    (
        CellCheck,
        Arc<tidepool_toolchain::checked_cell::CellProgram>,
    ),
    CellCheckFailure,
> {
    compile_cell_program_admitted_inner(
        req,
        admission,
        templates,
        CellProgramAudit::WorkCounts(expected_items),
    )
}

fn compile_cell_program_admitted_inner(
    req: CellCheckRequest<'_>,
    admission: Arc<super::RuntimeCellAdmission>,
    templates: &[TurnTemplate],
    #[cfg(test)] audit: CellProgramAudit,
) -> Result<
    (
        CellCheck,
        Arc<tidepool_toolchain::checked_cell::CellProgram>,
    ),
    CellCheckFailure,
> {
    validate_cell_admitted_request(&req, &admission)?;
    let planned = admission.plan_reservation().ok_or_else(|| {
        CompileError::ExtractFailed(
            "complete cell compilation requires its parser reservation".into(),
        )
    })?;
    let scratch = compiler_scratch_directory()?;
    let cell_path = scratch.path().join("cell.txt");
    let template_path = scratch.path().join("CellCheckTemplate.hs");
    let output_path = scratch.path().join("cell.cbor");
    std::fs::write(&cell_path, req.cell_text)?;
    std::fs::write(&template_path, req.template)?;
    let mut command = extract_cmd()?;
    command
        .input(&cell_path)
        .cell()
        .cell_template(&template_path)
        .cell_out(&output_path)
        .output_dir(scratch.path())
        .includes(req.include)
        .session_root(req.session_root)
        .inject_vals(req.inject_modules);
    if let Some(session) = req.session_id {
        command.session_incarnation(session.0.to_string());
    }
    for (index, template) in templates.iter().enumerate() {
        let path = scratch.path().join(format!("checked-template-{index}.hs"));
        std::fs::write(&path, &template.source)?;
        command.turn_template(template.kind.wire_name(), &path);
    }
    for (identity, generation) in admission.admitted_retained_imports() {
        command.retained_generation(extract_identity(identity), *generation);
    }
    let endpoint = bind_extract_cmd(&command)?;
    let specification = CheckedCellSpecification {
        admission_digest: admission.digest(),
        cell_source: req.cell_text.to_owned(),
        template_source: req.template.to_owned(),
        turn_templates: templates
            .iter()
            .map(|template| (template.kind.wire_name().into(), template.source.clone()))
            .collect(),
        injected_modules: req.inject_modules.to_vec(),
        reserved_declaration_modules: admission
            .reserved_generations()
            .iter()
            .map(|generation| tidepool_repr::SessionModule::lib(*generation).module_name())
            .collect(),
    };
    if specification.specification_digest() != admission.specification_digest() {
        return Err(CompileError::ExtractFailed(
            "cell compiler recipe differs from its runtime reservation".into(),
        )
        .into());
    }
    let include = req
        .include
        .iter()
        .map(|path| path.to_path_buf())
        .collect::<Vec<_>>();
    let offer = ModuleCandidateOffer::select_cell_program(
        &endpoint,
        &include,
        scratch.path(),
        req.exact_context.clone(),
        specification,
        admission
            .interfaces()
            .iter()
            .map(|interface| (interface.module(), interface.bytes_owned().clone()))
            .collect(),
        planned.compiler_specification(),
        &admission
            .interfaces()
            .iter()
            .map(|interface| interface.checked_artifact().clone())
            .collect::<Vec<_>>(),
        &admission.retained_declaration_projections(),
    )?;
    if let Some(root) = offer.checked_value_root() {
        command.session_root(root);
    }
    offer.apply_to(&mut command)?;
    crate::paths::apply_admitted_build_products_dir(&mut command, &endpoint);
    let diagnostics = CompilerDiagnosticCapture::start(scratch.path(), &command);
    let run = endpoint.execute(&command).map_err(|error| {
        offer.retain_execution_failure(scratch.path(), &command, map_notfound(error))
    })?;
    diagnostics.completed(scratch.path(), &command, run.success(), &run.output.stderr);
    let report =
        crate::diag::decode_extract_result(run.success(), &run.output.stdout, &run.output.stderr)
            .map_err(|error| {
            offer.retain_failure(scratch.path(), &command, &run.output.stderr, error)
        })?;
    #[cfg(test)]
    if matches!(audit, CellProgramAudit::Receipts) {
        audit_compiler_issued_item_receipts(&offer, scratch.path());
    }
    let program = offer.admit_cell_program(scratch.path()).map_err(|error| {
        offer.retain_failure(scratch.path(), &command, &run.output.stderr, error)
    })?;
    #[cfg(test)]
    if let CellProgramAudit::WorkCounts(expected_items) = audit {
        scaling_tests::assert_compiler_work(&program, &run.output.stderr, expected_items);
    }
    let mut checked = decode_cell_out(
        program.checked_cell().observations(),
        req.cell_text,
        req.compile_generation,
        req.compile_view_evidence,
    )?;
    checked.warnings = report.diagnostics;
    admission
        .prepare_declaration_projections(program.checked_cell())
        .map_err(|error| CompileError::ExtractFailed(error.to_string()))?;
    checked.authority = Some(program.checked_cell().clone());
    checked.admission = Some(admission);
    Ok((checked, program))
}

fn check_cell_impl(
    req: CellCheckRequest<'_>,
    admission: Option<Arc<super::RuntimeCellAdmission>>,
    admitted_templates: Option<&[TurnTemplate]>,
) -> Result<CellCheck, CellCheckFailure> {
    let temp = compiler_scratch_directory()?;
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
    if let Some(session_id) = req.session_id {
        cmd.session_incarnation(session_id.0.to_string());
    }
    if let Some(templates) = admitted_templates {
        for (i, tmpl) in templates.iter().enumerate() {
            let path = temp.path().join(format!("checked-template-{i}.hs"));
            std::fs::write(&path, &tmpl.source)?;
            cmd.turn_template(tmpl.kind.wire_name(), &path);
        }
    }
    let endpoint = bind_extract_cmd(&cmd)?;
    let include: Vec<_> = req.include.iter().map(|path| path.to_path_buf()).collect();
    let offer = if let Some(admission) = &admission {
        let context = req.exact_context.clone();
        let specification = CheckedCellSpecification {
            admission_digest: admission.digest(),
            cell_source: req.cell_text.into(),
            template_source: req.template.into(),
            turn_templates: admitted_templates
                .map(|templates| {
                    templates
                        .iter()
                        .map(|template| (template.kind.wire_name().into(), template.source.clone()))
                        .collect()
                })
                .unwrap_or_default(),
            injected_modules: req.inject_modules.to_vec(),
            reserved_declaration_modules: admission
                .reserved_generations()
                .iter()
                .map(|generation| tidepool_repr::SessionModule::lib(*generation).module_name())
                .collect(),
        };
        if specification.specification_digest() != admission.specification_digest() {
            return Err(CompileError::ExtractFailed(
                "cell body or compiler recipe differs from the retained actor specification".into(),
            )
            .into());
        }
        ModuleCandidateOffer::select_checked_cell(
            endpoint.identity().producer_bytes(),
            &include,
            temp.path(),
            context,
            specification,
            tidepool_toolchain::artifacts::CheckedCellPurpose::Authored,
            admission
                .interfaces()
                .iter()
                .map(|interface| (interface.module(), interface.bytes_owned().clone()))
                .collect(),
            &admission
                .interfaces()
                .iter()
                .map(|interface| interface.checked_artifact().clone())
                .collect::<Vec<_>>(),
            &admission.retained_declaration_projections(),
        )?
    } else {
        select_module_candidate_offer(
            endpoint.identity().producer_bytes(),
            &include,
            temp.path(),
            req.exact_context.clone(),
        )?
    };
    if let Some(root) = offer.checked_value_root() {
        cmd.session_root(root);
    }
    offer.apply_to(&mut cmd)?;
    crate::paths::apply_admitted_build_products_dir(&mut cmd, &endpoint);
    let diagnostics = CompilerDiagnosticCapture::start(temp.path(), &cmd);
    let run = endpoint
        .execute(&cmd)
        .map_err(|error| offer.retain_execution_failure(temp.path(), &cmd, map_notfound(error)))?;
    let output = &run.output;
    diagnostics.completed(temp.path(), &cmd, run.success(), &output.stderr);
    timing::log_interface_counts(&output.stderr);
    let report =
        match crate::diag::decode_extract_result(run.success(), &output.stdout, &output.stderr) {
            Ok(report) => report,
            Err(error) => {
                let items = match std::fs::read(&out_path) {
                    Ok(bytes) => Some(
                        decode_cell_out(
                            &bytes,
                            req.cell_text,
                            req.compile_generation,
                            req.compile_view_evidence,
                        )?
                        .items,
                    ),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                    Err(error) => return Err(error.into()),
                };
                return Err(CellCheckFailure {
                    error: offer.retain_failure(temp.path(), &cmd, &output.stderr, error),
                    items,
                });
            }
        };
    let bytes = std::fs::read(&out_path)?;
    let mut checked = decode_cell_out(
        &bytes,
        req.cell_text,
        req.compile_generation,
        req.compile_view_evidence,
    )?;
    if let Some(admission) = admission {
        // This owner validates receipts with the same-request original
        // declaration before admitting the final checking source.
        let authority = offer
            .admit_checked_cell(temp.path())
            .map_err(|error| offer.retain_failure(temp.path(), &cmd, &output.stderr, error))?;
        if authority.checked_source() != checked.checked_source {
            return Err(offer
                .retain_failure(
                    temp.path(),
                    &cmd,
                    &output.stderr,
                    CompileError::ExtractFailed(
                        "checked source differs from its admitted cell authority".into(),
                    ),
                )
                .into());
        }
        admission
            .prepare_declaration_projections(&authority)
            .map_err(|error| CompileError::ExtractFailed(error.to_string()))?;
        checked.authority = Some(authority);
        checked.admission = Some(admission);
    } else if req.exact_context.is_some() {
        let validated = (|| {
            let witnesses = offer.validate_exact_outputs(temp.path())?;
            let module =
                extract_module_name(&checked.checked_source).unwrap_or_else(|| "CellCheck".into());
            let source_path = temp.path().join(format!("{module}.hs"));
            if !witnesses
                .iter()
                .any(|witness| witness.matches_source(&source_path, &checked.checked_source))
            {
                return Err(CompileError::ExtractFailed(
                    "checked cell lacks its exact consumed source receipt".into(),
                ));
            }
            Ok::<_, CompileError>(())
        })();
        validated
            .map_err(|error| offer.retain_failure(temp.path(), &cmd, &output.stderr, error))?;
    }
    checked.warnings = report.diagnostics;
    Ok(checked)
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
    run_turn_with_admission(req, None)
}

/// Select the precompiled native item from the complete immutable program.
/// Runtime admission still owns the ordered cursor and exact live native leases.
pub fn consume_cell_program_item(
    admission: Arc<super::RuntimeCheckedItemAdmission>,
) -> Result<TurnResult, TurnFailure> {
    let program = admission.prefix().cell_program().ok_or_else(|| {
        CompileError::ExtractFailed("item admission has no complete cell program".into())
    })?;
    let item = program
        .items()
        .get(admission.item().index())
        .ok_or_else(|| {
            CompileError::ExtractFailed("item is outside its complete cell program".into())
        })?;
    let execution = item.native().ok_or_else(|| {
        CompileError::ExtractFailed("complete cell item has no native products".into())
    })?;
    if item.checked_item() != admission.item() || execution.generation() != admission.generation().0
    {
        return Err(CompileError::ExtractFailed(
            "prepared item differs from its original reservation".into(),
        )
        .into());
    }
    let result = decode_cell_program_turn(item, &admission)?;
    Ok(result)
}

fn decode_cell_program_turn(
    item: &tidepool_toolchain::checked_cell::CellProgramItem,
    admission: &Arc<super::RuntimeCheckedItemAdmission>,
) -> Result<TurnResult, CompileError> {
    let (turn_bytes, metadata_bytes, products, prepared) = (
        item.native_turn_bytes(),
        item.native_metadata_bytes(),
        item.native_products(),
        item.native().map(|proof| proof.target_owned()),
    );
    let missing =
        || CompileError::ExtractFailed("complete cell lacks its sealed output observations".into());
    let turn = decode_turn_out(turn_bytes.ok_or_else(missing)?)?;
    let (table, warnings) = read_metadata(metadata_bytes.ok_or_else(missing)?)?;
    let products = products.ok_or_else(missing)?;
    let prepared = prepared.ok_or_else(missing)?;
    let certification = TurnCertification {
        artifact_view: products.artifact_view.clone(),
        original_compile_input: None,
        groups: products.certified_groups.clone(),
        target_owners: products.pending_imports.clone(),
        package_interfaces: products.package_interfaces.clone(),
        recovery_products: products.recovery_products.clone(),
        purpose: TurnPurpose::execution(
            item.native().expect("native products preflighted").clone(),
            admission,
        )?,
    };
    certification.validate_checked_table(&table)?;
    match turn {
        DecodedTurnOut::Bind {
            binders,
            variant,
            bound,
            asks,
            wrapped_source,
        } => Ok(TurnResult::Bind {
            binders,
            variant,
            bound,
            wrapped_source,
            compiled: CompiledTurn {
                table,
                warnings,
                asks,
                prepared,
                certification: Some(certification),
            },
        }),
        DecodedTurnOut::Expr {
            variant,
            asks,
            wrapped_source,
        } => Ok(TurnResult::Expr {
            variant,
            wrapped_source,
            compiled: CompiledTurn {
                table,
                warnings,
                asks,
                prepared,
                certification: Some(certification),
            },
        }),
        _ => Err(CompileError::ExtractFailed(
            "complete native cell output has another item kind".into(),
        )),
    }
}

#[cfg(test)]
fn audit_compiler_issued_item_receipts(offer: &ModuleCandidateOffer, root: &Path) {
    use sha2::{Digest, Sha256};
    use tidepool_toolchain::certified_products::CertificationError;
    struct RestoreReceipt {
        path: PathBuf,
        original: Vec<u8>,
    }
    impl Drop for RestoreReceipt {
        fn drop(&mut self) {
            std::fs::write(&self.path, &self.original).expect("restore original compiler receipt");
        }
    }
    let before = tidepool_extract_cmd::extract_spawn_count();
    let valid = offer
        .admit_cell_program(root)
        .expect("unchanged original worker receipts must admit");
    let index = valid
        .items()
        .iter()
        .find(|item| item.native().is_some())
        .expect("witness controls require a real native entry")
        .checked_item()
        .index();
    let path = root.join(format!("item-{index}")).join("checked-item.cbor");
    let original = std::fs::read(&path).unwrap();
    let receipt: CborValue = ciborium::de::from_reader(original.as_slice()).unwrap();
    let fields = receipt.as_array().unwrap();
    assert_eq!(fields.len(), 9);
    assert_eq!(fields[0].as_text(), Some("TPEXACTITEM"));
    assert_eq!(fields[1].as_text(), Some("2"));
    assert_eq!(fields[8].as_array().unwrap().len(), 4);
    assert_eq!(index, 0, "this control starts with one executable segment");
    let module = fields[8].as_array().unwrap()[2].as_array().unwrap()[1]
        .as_text()
        .unwrap();
    let segment = root.join("segment-0");
    let source_path = segment.join(format!("{module}.hs"));
    let source = std::fs::read(&source_path).unwrap();
    let item_source = root.join("item-0").join(format!("{module}.hs"));
    assert_eq!(std::fs::read(&item_source).unwrap(), source);
    assert!(
        !root.join("item-0/.exact-compilations").exists(),
        "projection products must admit from their actual segment receipt owner"
    );
    struct RestoreDirectory {
        original: PathBuf,
        held: PathBuf,
    }
    impl Drop for RestoreDirectory {
        fn drop(&mut self) {
            std::fs::rename(&self.held, &self.original)
                .expect("restore original compiler receipt directory");
        }
    }
    let receipts = segment.join(".exact-compilations");
    let held = tempfile::tempdir_in(&segment).unwrap();
    let moved = RestoreDirectory {
        original: receipts.clone(),
        held: held.path().join("receipts"),
    };
    std::fs::rename(&receipts, &moved.held).unwrap();
    assert!(matches!(
        offer.admit_cell_program(root),
        Err(CompileError::ExtractFailed(_))
    ));
    drop(moved);
    offer
        .admit_cell_program(root)
        .expect("restored genuine segment receipts must admit");
    let restore_source = RestoreReceipt {
        path: source_path.clone(),
        original: source.clone(),
    };
    let mut changed_source = source;
    changed_source.extend_from_slice(b"\n-- changed after compilation\n");
    std::fs::write(&source_path, changed_source).unwrap();
    assert_ne!(
        std::fs::read(&item_source).unwrap(),
        std::fs::read(&source_path).unwrap()
    );
    assert!(matches!(
        offer.admit_cell_program(root),
        Err(CompileError::ExtractFailed(_))
    ));
    drop(restore_source);
    offer
        .admit_cell_program(root)
        .expect("restored consumed segment source must admit independently");
    let output_module = valid.items()[index]
        .native()
        .unwrap()
        .value_interface()
        .expect("this worker control produces an actual capture")
        .0;
    let products_path = root.join(format!("item-{index}/certified-products.cbor"));
    let products_original = std::fs::read(&products_path).unwrap();
    let products: CborValue = ciborium::de::from_reader(products_original.as_slice()).unwrap();
    fn captured_owner(row: &CborValue) -> (&str, &str) {
        let fields = row.as_array().unwrap();
        (fields[0].as_text().unwrap(), fields[1].as_text().unwrap())
    }
    let value_rows = products.as_array().unwrap()[6].as_array().unwrap()[3]
        .as_array()
        .unwrap();
    let output_index = value_rows
        .iter()
        .position(|row| row.as_array().unwrap()[1].as_text() == Some(output_module))
        .expect("the finalization packet carries the produced capture type");
    let declaration = valid
        .items()
        .iter()
        .position(|item| item.native().is_none())
        .expect("future-output control requires a genuine declaration barrier");
    assert!(declaration > index);
    let future = valid.items()[declaration + 1..]
        .iter()
        .find(|item| {
            item.native()
                .and_then(|native| native.value_interface())
                .is_some()
        })
        .expect("the later segment must produce a real reserved capture");
    let future_module = future.native().unwrap().value_interface().unwrap().0;
    let future_directory = root.join(format!("item-{}", future.checked_item().index()));
    let future_bytes = std::fs::read(future_directory.join("certified-products.cbor")).unwrap();
    let future_products: CborValue = ciborium::de::from_reader(future_bytes.as_slice()).unwrap();
    let future_row = future_products.as_array().unwrap()[6].as_array().unwrap()[3]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row.as_array().unwrap()[1].as_text() == Some(future_module))
        .expect("the later capture must have genuine completed type evidence")
        .clone();
    let same_segment_future = valid.items()[index + 1..declaration]
        .iter()
        .find(|item| {
            item.native()
                .and_then(|native| native.value_interface())
                .is_some()
        })
        .expect("the first segment must contain a later genuine capture");
    let same_segment_module = same_segment_future
        .native()
        .unwrap()
        .value_interface()
        .unwrap()
        .0;
    let same_segment_directory = root.join(format!(
        "item-{}",
        same_segment_future.checked_item().index()
    ));
    let same_segment_bytes =
        std::fs::read(same_segment_directory.join("certified-products.cbor")).unwrap();
    let same_segment_products: CborValue =
        ciborium::de::from_reader(same_segment_bytes.as_slice()).unwrap();
    let same_segment_row = same_segment_products.as_array().unwrap()[6]
        .as_array()
        .unwrap()[3]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row.as_array().unwrap()[1].as_text() == Some(same_segment_module))
        .expect("the later same-segment capture must have genuine completed type evidence")
        .clone();
    assert_ne!(same_segment_module, output_module);
    assert!(value_rows
        .iter()
        .all(|row| row.as_array().unwrap()[1].as_text() != Some(same_segment_module)));
    assert_ne!(future_module, output_module);
    assert!(
        value_rows
            .iter()
            .all(|row| { row.as_array().unwrap()[1].as_text() != Some(future_module) }),
        "the first segment must not already emit later capture evidence"
    );
    for mutation in 0..5 {
        let restore_products = RestoreReceipt {
            path: products_path.clone(),
            original: products_original.clone(),
        };
        let mut changed = products.clone();
        let rows = changed.as_array_mut().unwrap()[6].as_array_mut().unwrap()[3]
            .as_array_mut()
            .unwrap();
        let mut restore_payload = None;
        let mut foreign_payloads = None;
        match mutation {
            0 => {
                rows.remove(output_index);
            }
            1 | 3 | 4 => {
                let mut foreign = match mutation {
                    3 => future_row.clone(),
                    4 => same_segment_row.clone(),
                    _ => rows[output_index].clone(),
                };
                let row = foreign.as_array_mut().unwrap();
                if mutation == 1 {
                    row[1] = CborValue::Text(format!("Tidepool.Session.Val.G{}", u64::MAX));
                }
                let parent = products_path.parent().unwrap();
                let payload_parent: &Path = match mutation {
                    3 => &future_directory,
                    4 => &same_segment_directory,
                    _ => parent,
                };
                let directory = tempfile::tempdir_in(parent).unwrap();
                for (field, name) in [(2, "foreign.hi"), (5, "foreign.packages.cbor")] {
                    let original = payload_parent.join(row[field].as_text().unwrap());
                    std::fs::copy(original, directory.path().join(name)).unwrap();
                    row[field] = CborValue::Text(format!(
                        "{}/{name}",
                        directory.path().file_name().unwrap().to_str().unwrap()
                    ));
                }
                foreign_payloads = Some(directory);
                rows.push(foreign);
            }
            2 => {
                let row = rows[output_index].as_array_mut().unwrap();
                let payload = products_path
                    .parent()
                    .unwrap()
                    .join(row[2].as_text().unwrap());
                let original = std::fs::read(&payload).unwrap();
                let mut substituted = original.clone();
                assert!(!substituted.is_empty());
                substituted[0] ^= 1;
                row[3] = CborValue::Text(
                    Sha256::digest(&substituted)
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect(),
                );
                restore_payload = Some(RestoreReceipt {
                    path: payload.clone(),
                    original,
                });
                std::fs::write(payload, substituted).unwrap();
            }
            _ => unreachable!(),
        }
        // Keep the producer's strict owner order so each mutation reaches its
        // type-evidence guard rather than the envelope ordering check.
        rows.sort_by(|left, right| captured_owner(left).cmp(&captured_owner(right)));
        assert!(rows
            .windows(2)
            .all(|pair| captured_owner(&pair[0]) < captured_owner(&pair[1])));
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&changed, &mut bytes).unwrap();
        std::fs::write(&products_path, bytes).unwrap();
        let refusal = offer.admit_cell_program(root).expect_err(
            "missing, foreign, future or substituted produced type evidence must refuse",
        );
        assert!(
            matches!(&refusal, CompileError::CompilerEvidence(error)
            if matches!(error.as_ref(), CertificationError::Mismatch(_))),
            "produced type mutation {mutation} must reach its evidence guard: {refusal:?}"
        );
        if let Some(expected) = match mutation {
            0 => Some("produced value type output is missing"),
            1 | 3 | 4 => Some("captured value was not selected by the request"),
            2 => Some("produced value type output differs from its reserved capture"),
            _ => None,
        } {
            assert!(
                matches!(&refusal, CompileError::CompilerEvidence(error)
                if matches!(error.as_ref(), CertificationError::Mismatch(message) if *message == expected)),
                "produced type mutation {mutation} must reach its exact owner guard: {refusal:?}"
            );
        }
        drop(restore_payload);
        drop(restore_products);
        drop(foreign_payloads);
        offer
            .admit_cell_program(root)
            .expect("restored produced type evidence must admit independently");
    }
    for field in 0..4 {
        let restore = RestoreReceipt {
            path: path.clone(),
            original: original.clone(),
        };
        let mut changed = receipt.clone();
        let proof = changed.as_array_mut().unwrap()[8].as_array_mut().unwrap();
        match field {
            0 => proof[0] = CborValue::Text("0".repeat(64)),
            1 | 2 => {
                let identity = proof[field].as_array_mut().unwrap();
                let occurrence = identity[2].as_text().unwrap();
                identity[2] = CborValue::Text(format!("{occurrence}_substituted"));
            }
            3 => {
                let ordinal = u32::try_from(proof[3].as_integer().unwrap()).unwrap();
                proof[3] = CborValue::Integer((u64::from(ordinal ^ 1)).into());
            }
            _ => unreachable!(),
        }
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&changed, &mut bytes).unwrap();
        assert_ne!(bytes, original);
        std::fs::write(&path, bytes).unwrap();
        let refusal = offer
            .admit_cell_program(root)
            .expect_err("a substituted ITEM2 witness must refuse");
        assert!(
            matches!(&refusal, CompileError::CompilerEvidence(error)
            if matches!(error.as_ref(), CertificationError::Mismatch(_))),
            "field {field} must reach the typed native-entry seal guard: {refusal:?}"
        );
        drop(restore);
        offer
            .admit_cell_program(root)
            .expect("restored actual worker bytes must admit independently");
    }
    let cell_path = root.join("checked-cell.cbor");
    let observations_path = root.join("cell.cbor");
    let cell_original = std::fs::read(&cell_path).unwrap();
    let observations_original = std::fs::read(&observations_path).unwrap();
    let cell: CborValue = ciborium::de::from_reader(cell_original.as_slice()).unwrap();
    let observations: CborValue =
        ciborium::de::from_reader(observations_original.as_slice()).unwrap();
    assert_eq!(cell.as_array().unwrap().len(), 12);
    assert_eq!(cell.as_array().unwrap()[1].as_text(), Some("4"));
    let first_signature = cell.as_array().unwrap()[8]
        .as_array()
        .unwrap()
        .first()
        .expect("the real cell must publish descriptor-derived capture types")
        .clone();
    let payload = observations.as_array().unwrap()[2].as_array().unwrap();
    assert!(
        payload[1].as_array().unwrap().is_empty(),
        "PROGRAM4 must not recreate synthetic pins"
    );
    let first_observation = payload[4]
        .as_array()
        .unwrap()
        .first()
        .expect("the actual final expression must publish its typed observation descriptor")
        .clone();
    for mutation in 0..9 {
        let restore_cell = RestoreReceipt {
            path: cell_path.clone(),
            original: cell_original.clone(),
        };
        let restore_observations = RestoreReceipt {
            path: observations_path.clone(),
            original: observations_original.clone(),
        };
        let mut changed_cell = cell.clone();
        let mut changed_observations = observations.clone();
        match mutation {
            0..=2 => {
                let signatures = changed_cell.as_array_mut().unwrap()[8]
                    .as_array_mut()
                    .unwrap();
                match mutation {
                    0 => {
                        signatures.remove(0);
                    }
                    1 => signatures.push(first_signature.clone()),
                    2 => {
                        let mut extra = first_signature.clone();
                        extra.as_array_mut().unwrap()[1] =
                            CborValue::Text("unowned:capture".into());
                        signatures.push(extra);
                    }
                    _ => unreachable!(),
                }
            }
            3 => {
                changed_observations.as_array_mut().unwrap()[2]
                    .as_array_mut()
                    .unwrap()[1]
                    .as_array_mut()
                    .unwrap()
                    .push(CborValue::Array(vec![
                        CborValue::Text("obsolete_pin".into()),
                        CborValue::Text("unused".into()),
                        CborValue::Array(Vec::new()),
                    ]));
            }
            4..=8 => {
                let expressions = changed_observations.as_array_mut().unwrap()[2]
                    .as_array_mut()
                    .unwrap()[4]
                    .as_array_mut()
                    .unwrap();
                match mutation {
                    4 => {
                        expressions.remove(0);
                    }
                    5 => expressions.push(first_observation.clone()),
                    6 => {
                        let mut extra = first_observation.clone();
                        extra.as_array_mut().unwrap()[0] = CborValue::Text("unowned_entry".into());
                        expressions.push(extra);
                    }
                    7 => {
                        expressions[0].as_array_mut().unwrap()[0] = CborValue::Text(
                            valid.items()[index]
                                .native()
                                .unwrap()
                                .typed_entry()
                                .unwrap()
                                .entry()
                                .occurrence
                                .clone(),
                        )
                    }
                    8 => {
                        expressions[0].as_array_mut().unwrap()[1] =
                            CborValue::Text("unknown_lift".into())
                    }
                    _ => unreachable!(),
                }
            }
            _ => unreachable!(),
        }
        let mut observation_bytes = Vec::new();
        ciborium::ser::into_writer(&changed_observations, &mut observation_bytes).unwrap();
        // Keep the ordinary byte seal consistent so observation corruption
        // reaches the new typed metadata ownership guard.
        changed_cell.as_array_mut().unwrap()[6] = CborValue::Text(
            Sha256::digest(&observation_bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        );
        let mut cell_bytes = Vec::new();
        ciborium::ser::into_writer(&changed_cell, &mut cell_bytes).unwrap();
        assert!(cell_bytes != cell_original || observation_bytes != observations_original);
        std::fs::write(&observations_path, observation_bytes).unwrap();
        std::fs::write(&cell_path, cell_bytes).unwrap();
        let refusal = offer
            .admit_cell_program(root)
            .expect_err("unowned typed metadata must refuse");
        assert!(
            matches!(&refusal, CompileError::CompilerEvidence(error)
            if matches!(error.as_ref(), CertificationError::Mismatch("typed segment metadata"))),
            "mutation {mutation} must reach the typed metadata ownership guard: {refusal:?}"
        );
        drop(restore_observations);
        drop(restore_cell);
        offer
            .admit_cell_program(root)
            .expect("restored genuine typed metadata must admit");
    }
    assert_eq!(
        tidepool_extract_cmd::extract_spawn_count(),
        before,
        "receipt controls revalidate original outputs without another compiler request"
    );
}

/// Compile only the next item of a runtime-owned completed prefix. Body,
/// verdict, annotation Names and wrapper recipes come from its same check.
pub fn run_checked_item(
    req: TurnRequest<'_>,
    item_admission: Arc<super::RuntimeCheckedItemAdmission>,
) -> Result<TurnResult, TurnFailure> {
    let prefix = item_admission.prefix();
    let item = item_admission.item();
    let snapshot = item_admission.snapshot();
    let view = snapshot.view();
    let verdict = req.verdict.as_ref().ok_or_else(|| {
        CompileError::ExtractFailed("checked item requires its sealed verdict".into())
    })?;
    let kind = match verdict.kind {
        TurnKind::Decl => tidepool_toolchain::checked_cell::CheckedItemKind::Declaration,
        TurnKind::Bind => tidepool_toolchain::checked_cell::CheckedItemKind::Bind,
        TurnKind::Expr => tidepool_toolchain::checked_cell::CheckedItemKind::Expression,
    };
    if req.turn_text != item.source()
        || kind != item.kind()
        || verdict.binders != item.binders()
        || req.session_id != Some(view.session())
        || req.session_root != view.session_root()
        || req.exact_context != view.exact_compile_context()
        || req.inject_modules != snapshot.compiler_prefix().injected_modules()
        || req.gen != item_admission.generation().0
        || req.target.is_some()
        || item.admission_digest() != prefix.admission().digest()
        || item.index() != snapshot.compiler_prefix().next_item()
    {
        return Err(CompileError::ExtractFailed(
            "checked item request differs from its protected admission and completed prefix".into(),
        )
        .into());
    }
    if prefix.cell_program().is_some() {
        return consume_cell_program_item(item_admission);
    }
    run_turn_with_admission(req, Some(item_admission))
}

/// Pure original-type rendering recipe; it publishes no resident value interface.
pub fn assemble_activation_preview_module(budget: u64) -> String {
    let preamble = "{-# LANGUAGE DataKinds, FlexibleContexts #-}\nmodule Input where\nimport Control.Monad.Freer (Eff)\n";
    let preamble = super::insert_preamble_imports(
        &with_resume_import(preamble),
        "qualified Tidepool.Inspection as TidepoolInspection",
    );
    let mut source = preamble;
    source.push_str(&format!(
        "__activationPreview :: TidepoolActivationInput -> Eff '[] ({TEXT_ALIAS}.Text, Bool)\n\
         __activationPreview __activationInput = pure ({{{{ACTIVATION_PREVIEW}}}})\n\
         __tidepoolActivationConstraint :: TidepoolInspection.WorkbenchDisplay value => value -> ()\n\
         __tidepoolActivationConstraint _ = ()\n\
         __activationBudget :: Int\n\
         __activationBudget = {budget}\n\
         {PREPARED_SCAFFOLD_TARGET} input = {RESUME_ALIAS}.settle (__activationPreview input)\n"
    ));
    source.push_str(&prepared_resume_apply_binding());
    source
}

/// Compile a fresh turn or a runtime-admitted recipe whose native products
/// were not already prepared by the complete cell program.
#[tracing::instrument(
    name = "turn_compile",
    level = "info",
    skip_all,
    fields(
        kind = req.verdict.as_ref().map_or("", |verdict| turn_kind_wire_name(verdict.kind)),
        checked = checked.is_some(),
    )
)]
fn run_turn_with_admission(
    req: TurnRequest<'_>,
    checked: Option<Arc<super::RuntimeCheckedItemAdmission>>,
) -> Result<TurnResult, TurnFailure> {
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

    let temp = compiler_scratch_directory()?;
    let snapshot = checked.as_ref().map(|admission| admission.snapshot());
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
    if let Some(session_id) = req.session_id {
        cmd.session_incarnation(session_id.0.to_string());
    }
    if let Some(target) = req.target {
        cmd.target(target);
    }
    if let Some(arg) = verdict_arg {
        cmd.turn_verdict(arg);
    }
    if let Some(snapshot) = &snapshot {
        for (identity, generation) in snapshot.actual_retained_imports() {
            cmd.retained_generation(extract_identity(identity), generation);
        }
    } else {
        for (identity, generation) in req.retained_imports {
            cmd.retained_generation(extract_identity(identity), *generation);
        }
    }
    let settled_bindings = snapshot
        .map(|snapshot| {
            snapshot
                .settled_native_bindings()
                .map(|(name, identity, generation, id)| {
                    (name.to_owned(), identity.clone(), generation, id)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let endpoint = bind_extract_cmd(&cmd)?;
    let include: Vec<_> = req.include.iter().map(|path| path.to_path_buf()).collect();
    let offer = if let Some(admission) = &checked {
        let templates = req
            .templates
            .iter()
            .map(|template| (template.kind.wire_name().into(), template.source.clone()))
            .collect::<Vec<_>>();
        ModuleCandidateOffer::select_checked_item(
            endpoint.identity().producer_bytes(),
            &include,
            temp.path(),
            req.exact_context.clone(),
            admission.item().clone(),
            admission.snapshot().compiler_prefix().clone(),
            admission.digest(),
            req.gen,
            admission.observation_name(),
            &templates,
            settled_bindings,
        )?
    } else if req.exact_context.is_none() {
        ModuleCandidateOffer::select_admitted(&endpoint, &include, temp.path())?
    } else {
        select_module_candidate_offer(
            endpoint.identity().producer_bytes(),
            &include,
            temp.path(),
            req.exact_context.clone(),
        )?
    };
    if let Some(root) = offer.checked_value_root() {
        cmd.session_root(root);
    }
    offer.apply_to(&mut cmd)?;
    crate::paths::apply_admitted_build_products_dir(&mut cmd, &endpoint);
    let ordinary_admitted = req.exact_context.is_none() && checked.is_none();
    enum TurnCompilerOutput {
        Admitted(tidepool_toolchain::artifacts::AdmittedTurnOutput),
        Direct(tidepool_extract_cmd::ExtractRun),
    }
    let diagnostics =
        (!ordinary_admitted).then(|| CompilerDiagnosticCapture::start(temp.path(), &cmd));
    let run = if ordinary_admitted {
        TurnCompilerOutput::Admitted(offer.execute_admitted_turn(endpoint, &mut cmd)?)
    } else {
        TurnCompilerOutput::Direct(endpoint.execute(&cmd).map_err(|error| {
            offer.retain_execution_failure(temp.path(), &cmd, map_notfound(error))
        })?)
    };
    let (output, elapsed, output_dir) = match &run {
        TurnCompilerOutput::Admitted(run) => {
            (run.compiler_output(), run.elapsed(), run.directory())
        }
        TurnCompilerOutput::Direct(run) => (&run.output, run.elapsed, temp.path()),
    };
    if let Some(diagnostics) = diagnostics {
        diagnostics.completed(output_dir, &cmd, output.status.success(), &output.stderr);
    }
    timing::record_stage(
        timing::NO_NODE,
        timing::NO_ROUND,
        timing::STAGE_EXTRACT_SPAWN,
        elapsed,
        0,
    );
    timing::log_interface_counts(&output.stderr);
    let stderr = String::from_utf8_lossy(&output.stderr);
    // A failed compile is still a real spawn — attribute its extract phases
    // the same as a successful one, before the early return below.
    forward_extract_timing(&stderr, "extract");
    // Default-on per-compile summary + gated per-module breakdown
    // (compile-attribution lane): this `--turn` spawn goes through the SAME
    // `Tidepool.GhcPipeline.runCompile` skeleton `artifacts.rs::extract_and_read`
    // instruments (`runTurnMode` → `runPipelineSessionSelected` → `runCompile` — see
    // `bridge/haskell/app/Main.hs`), but reads its own `stderr` here rather than
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
        crate::diag::decode_extract_result(output.status.success(), &output.stdout, &output.stderr)
    {
        let attempted_source = std::fs::read_to_string(output_dir.join("turn-attempt.hs")).ok();
        return Err(TurnFailure {
            error: offer.retain_failure(output_dir, &cmd, &output.stderr, error),
            attempted_source,
        });
    }

    let decoded = match &run {
        TurnCompilerOutput::Admitted(run) => decode_admitted_turn_output(run, checked.as_ref()),
        TurnCompilerOutput::Direct(_) => {
            decode_turn_output_dir(output_dir, &offer, checked.as_ref())
        }
    };
    let mut result = decoded.map_err(|error| TurnFailure {
        error: offer.retain_failure(
            output_dir,
            &cmd,
            &output.stderr,
            match error {
                CompileError::ExtractFailed(detail) if !stderr.trim().is_empty() => {
                    CompileError::ExtractFailed(format!("{detail}\nworker stderr:\n{stderr}"))
                }
                other => other,
            },
        ),
        attempted_source: std::fs::read_to_string(output_dir.join("turn-attempt.hs"))
            .ok()
            .or_else(|| {
                let bytes = std::fs::read(output_dir.join("turn.cbor")).ok()?;
                match decode_turn_out(&bytes).ok()? {
                    DecodedTurnOut::Bind { wrapped_source, .. }
                    | DecodedTurnOut::Expr { wrapped_source, .. } => Some(wrapped_source),
                    DecodedTurnOut::Decl { .. } => None,
                }
            }),
    })?;
    let compile_identity = match &run {
        TurnCompilerOutput::Admitted(run) => run.original_compile_input(),
        TurnCompilerOutput::Direct(_) => None,
    };
    if let Some(identity) = compile_identity {
        let compiled = match &mut result {
            TurnResult::Bind { compiled, .. } | TurnResult::Expr { compiled, .. } => compiled,
            TurnResult::Decl(_) => {
                return Err(CompileError::ExtractFailed(
                    "compiler input proof has no native turn".into(),
                )
                .into())
            }
        };
        let certification = compiled.certification.as_mut().ok_or_else(|| {
            CompileError::ExtractFailed("compiler input proof has no certified products".into())
        })?;
        if !identity.matches_bundle(
            &compiled.prepared,
            &certification.groups,
            &certification.target_owners,
            &certification.package_interfaces,
            &compiled.table,
            &compiled.asks,
        ) {
            return Err(CompileError::ExtractFailed(
                "admitted compiler output bundle changed before decoding".into(),
            )
            .into());
        }
        certification.original_compile_input = Some(Arc::clone(identity));
    }
    if checked.is_some() {
        let compiled = match &result {
            TurnResult::Bind { compiled, .. } | TurnResult::Expr { compiled, .. } => compiled,
            TurnResult::Decl(_) => {
                return Err(
                    CompileError::ExtractFailed("checked declaration is unproved".into()).into(),
                )
            }
        };
        let certification = compiled.certification.as_ref().ok_or_else(|| {
            CompileError::ExtractFailed("checked item lacks sealed native products".into())
        })?;
        if let Some((module, bytes)) = certification
            .checked_execution()
            .and_then(|execution| execution.value_interface())
        {
            let generation = module
                .strip_prefix("Tidepool.Session.Val.G")
                .and_then(|suffix| suffix.parse::<u64>().ok())
                .ok_or_else(|| {
                    CompileError::ExtractFailed("checked binding has no exact Val.G owner".into())
                })?;
            let output = req.session_root.join(
                tidepool_repr::SessionModule::val(tidepool_repr::Generation(generation))
                    .relative_hi_path(),
            );
            if let Some(parent) = output.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(output, bytes)?;
        }
    }
    Ok(result)
}

pub(super) fn select_module_candidate_offer(
    producer: &[u8],
    include: &[PathBuf],
    scratch: &Path,
    context: Option<Arc<tidepool_toolchain::declaration_join::ExactCompileContext>>,
) -> Result<ModuleCandidateOffer, CompileError> {
    match context {
        Some(context) => {
            ModuleCandidateOffer::select_in_context(producer, include, scratch, context)
        }
        None => ModuleCandidateOffer::select(producer, include, scratch),
    }
}

/// Construct the runtime observation from the original immutable admitted
/// bundle. The exposed directory is retained only for compile diagnostics.
fn decode_admitted_turn_output(
    output: &tidepool_toolchain::artifacts::AdmittedTurnOutput,
    admission: Option<&Arc<super::RuntimeCheckedItemAdmission>>,
) -> Result<TurnResult, CompileError> {
    let missing =
        || CompileError::ExtractFailed("admitted turn lacks retained observations".into());
    let turn = decode_turn_out(output.turn_bytes().ok_or_else(missing)?)?;
    match turn {
        DecodedTurnOut::Decl {
            binders,
            items,
            source,
        } => {
            if output.native_output().is_some() {
                return Err(CompileError::ExtractFailed(
                    "declaration has a native output".into(),
                ));
            }
            Ok(TurnResult::Decl(DeclarationReceipt {
                binders,
                items,
                source,
            }))
        }
        DecodedTurnOut::Bind {
            binders,
            variant,
            bound,
            asks,
            wrapped_source,
        } => {
            let compiled = retained_compiled_turn(output, asks, &wrapped_source, admission)?;
            Ok(TurnResult::Bind {
                binders,
                variant,
                bound,
                compiled,
                wrapped_source,
            })
        }
        DecodedTurnOut::Expr {
            variant,
            asks,
            wrapped_source,
        } => {
            let compiled = retained_compiled_turn(output, asks, &wrapped_source, admission)?;
            Ok(TurnResult::Expr {
                variant,
                compiled,
                wrapped_source,
            })
        }
    }
}

fn retained_compiled_turn(
    output: &tidepool_toolchain::artifacts::AdmittedTurnOutput,
    asks: Vec<YieldSite>,
    wrapped_source: &str,
    admission: Option<&Arc<super::RuntimeCheckedItemAdmission>>,
) -> Result<CompiledTurn, CompileError> {
    let native = output.native_output().ok_or_else(|| {
        CompileError::ExtractFailed("admitted native turn has no retained bundle".into())
    })?;
    if native.source() != wrapped_source {
        return Err(CompileError::ExtractFailed(
            "admitted native source differs from observation".into(),
        ));
    }
    compiled_native_output(
        native.table(),
        native.warnings(),
        asks,
        native.target_owned(),
        native.products(),
        admission,
    )
}

fn compiled_native_output(
    table: &DataConTable,
    warnings: &MetaWarnings,
    asks: Vec<YieldSite>,
    prepared: Arc<PreparedProgram>,
    products: Option<&tidepool_toolchain::artifacts::SealedTurnProducts>,
    admission: Option<&Arc<super::RuntimeCheckedItemAdmission>>,
) -> Result<CompiledTurn, CompileError> {
    let compiled = CompiledTurn {
        table: table.clone(),
        warnings: warnings.clone(),
        asks,
        prepared,
        certification: products
            .map(|products| -> Result<_, CompileError> {
                Ok(TurnCertification {
                    artifact_view: products.artifact_view.clone(),
                    original_compile_input: products.original_compile_input.clone(),
                    groups: products.certified_groups.clone(),
                    target_owners: products.pending_imports.clone(),
                    package_interfaces: products.package_interfaces.clone(),
                    recovery_products: products.recovery_products.clone(),
                    purpose: TurnPurpose::from_sealed(products.checked.as_ref(), admission)?,
                })
            })
            .transpose()?,
    };
    if let Some(certification) = &compiled.certification {
        certification.validate_checked_table(&compiled.table)?;
    }
    Ok(compiled)
}

/// Decode one item's full output directory into a [`TurnResult`]: the
/// `TurnOut` CBOR sidecar (`turn.cbor`) plus, for a `Bind`/`Expr` verdict,
/// `result.cbor`/`meta.cbor` off the SAME directory.
fn decode_turn_output_dir(
    dir: &Path,
    offer: &ModuleCandidateOffer,
    admission: Option<&Arc<super::RuntimeCheckedItemAdmission>>,
) -> Result<TurnResult, CompileError> {
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
            let compiled = read_compiled_turn(dir, asks, &wrapped_source, offer, admission)?;
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
            let compiled = read_compiled_turn(dir, asks, &wrapped_source, offer, admission)?;
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
    wrapped_source: &str,
    offer: &ModuleCandidateOffer,
    admission: Option<&Arc<super::RuntimeCheckedItemAdmission>>,
) -> Result<CompiledTurn, CompileError> {
    let meta_path = output_dir.join("meta.cbor");
    if !meta_path.exists() {
        return Err(CompileError::MissingOutput(meta_path));
    }
    let cbor_read_start = std::time::Instant::now();
    let meta_bytes = std::fs::read(&meta_path)?;
    let cbor_read_bytes = meta_bytes.len() as u64;
    timing::record_stage(
        timing::NO_NODE,
        timing::NO_ROUND,
        timing::STAGE_CBOR_READ,
        cbor_read_start.elapsed(),
        cbor_read_bytes,
    );

    let deserialize_start = std::time::Instant::now();
    let (table, warnings) = read_metadata(&meta_bytes)?;
    timing::record_stage(
        timing::NO_NODE,
        timing::NO_ROUND,
        timing::STAGE_CBOR_DESERIALIZE,
        deserialize_start.elapsed(),
        0,
    );
    // Runtime unresolved-error naming — see lib.rs twin sites.

    let prepared = read_prepared_program(output_dir)?;
    let module = extract_module_name(wrapped_source).unwrap_or_else(|| "Input".into());
    let source_path = output_dir.join(format!("{module}.hs"));
    let sealed = seal_turn_outputs(
        offer,
        output_dir,
        &source_path,
        wrapped_source,
        &prepared,
        PREPARED_SCAFFOLD_TARGET,
    )?;

    let compiled = CompiledTurn {
        table,
        warnings,
        asks,
        prepared,
        certification: sealed
            .map(|sealed| -> Result<_, CompileError> {
                Ok(TurnCertification {
                    artifact_view: sealed.artifact_view,
                    original_compile_input: sealed.original_compile_input,
                    groups: sealed.certified_groups,
                    target_owners: sealed.pending_imports,
                    package_interfaces: sealed.package_interfaces,
                    recovery_products: sealed.recovery_products,
                    purpose: TurnPurpose::from_sealed(sealed.checked.as_ref(), admission)?,
                })
            })
            .transpose()?,
    };
    if let Some(certification) = &compiled.certification {
        certification.validate_checked_table(&compiled.table)?;
    }
    Ok(compiled)
}

/// Read the prepared-STG program the worker wrote for this turn. A requested
/// program that is absent is a missing output.
fn read_prepared_program(output_dir: &Path) -> Result<Arc<PreparedProgram>, CompileError> {
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
        .map(Arc::new)
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

fn cbor_expect_bool(v: &CborValue, what: &str) -> Result<bool, CompileError> {
    match v {
        CborValue::Bool(value) => Ok(*value),
        other => Err(cbor_shape_error(what, "boolean", other)),
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

pub(super) fn decode_bound_binder(v: &CborValue) -> Result<BoundBinder, CompileError> {
    let arr = cbor_expect_array_len(v, 7, "BoundBinder")?;
    let name = cbor_expect_text(&arr[0], "BoundBinder name")?.to_string();
    let var_id = cbor_as_u64(&arr[1], "BoundBinder varId")?;
    let module = cbor_expect_text(&arr[2], "BoundBinder module")?.to_string();
    let tier = match cbor_expect_text(&arr[3], "BoundBinder tier")? {
        "RetainOpaque" => ValueTier::RetainOpaque,
        "ForceData" => ValueTier::ForceData,
        other => {
            return Err(CompileError::ExtractFailed(format!(
                "TurnOut CBOR: unknown BoundBinder tier {other:?}"
            )))
        }
    };
    let type_display = cbor_expect_text(&arr[4], "BoundBinder typeDisplay")?.to_string();
    let root_head = match &arr[5] {
        CborValue::Null => None,
        head => {
            let head = cbor_expect_array_len(head, 3, "BoundBinder nominal root")?;
            Some(NominalHead {
                unit: cbor_expect_text(&head[0], "BoundBinder root unit")?.to_owned(),
                module: cbor_expect_text(&head[1], "BoundBinder root module")?.to_owned(),
                name: cbor_expect_text(&head[2], "BoundBinder root name")?.to_owned(),
            })
        }
    };
    let host_authority = match &arr[6] {
        CborValue::Null => None,
        CborValue::Text(authority) => Some(match authority.as_str() {
            "JsonValue" => HostBindingAuthority::JsonValue,
            "Text" => HostBindingAuthority::Text,
            "CommandJob" => HostBindingAuthority::CommandJob,
            other => {
                return Err(CompileError::ExtractFailed(format!(
                    "TurnOut CBOR: unknown BoundBinder host authority {other:?}"
                )))
            }
        }),
        _ => {
            return Err(CompileError::ExtractFailed(
                "TurnOut CBOR: BoundBinder host authority must be text or null".into(),
            ))
        }
    };
    Ok(BoundBinder {
        name,
        var_id,
        module,
        tier,
        type_display,
        root_head,
        host_authority,
    })
}

fn decode_bound_binders(v: &CborValue) -> Result<Vec<BoundBinder>, CompileError> {
    cbor_expect_array(v, "boundBinders")?
        .iter()
        .map(decode_bound_binder)
        .collect()
}

pub(super) fn decode_source_prologue(value: &CborValue) -> Result<SourcePrologue, CompileError> {
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

pub(super) fn decode_cell_out(
    bytes: &[u8],
    cell_text: &str,
    compile_generation: u64,
    compile_view_evidence: &str,
) -> Result<CellCheck, CompileError> {
    let value: CborValue = ciborium::de::from_reader(bytes).map_err(|error| {
        CompileError::ExtractFailed(format!("CellOut CBOR: malformed: {error}"))
    })?;
    let envelope = cbor_expect_array_len(&value, 3, "cell observations envelope")?;
    if cbor_expect_text(&envelope[0], "cell observations magic")? != "TPCELLOBSERVATIONS"
        || cbor_as_usize(&envelope[1], "cell observations version")? != 3
    {
        return Err(CompileError::ExtractFailed(
            "unsupported cell observations version".into(),
        ));
    }
    let root = cbor_expect_array_len(&envelope[2], 5, "CellOut")?;
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
    let expression_plans = cbor_expect_array(&root[4], "cell expression plans")?
        .iter()
        .map(decode_checked_expression_plan)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(CellCheck {
        items,
        pins,
        checked_source,
        checked_cell_text: cell_text.to_owned(),
        compile_generation,
        compile_view_evidence: compile_view_evidence.to_owned(),
        expression_plans,
        prologue: decode_source_prologue(&root[3])?,
        warnings: Vec::new(),
        authority: None,
        admission: None,
    })
}

fn decode_checked_expression_plan(
    value: &CborValue,
) -> Result<CheckedExpressionPlan, CompileError> {
    let fields = cbor_expect_array_len(value, 4, "checked expression plan")?;
    let lift = match cbor_expect_text(&fields[1], "expression lift")? {
        "effectful" => ExpressionLift::Effectful,
        "pure" => ExpressionLift::Pure,
        other => {
            return Err(CompileError::ExtractFailed(format!(
                "CellOut CBOR: unknown expression lift {other:?}"
            )))
        }
    };
    Ok(CheckedExpressionPlan {
        key: cbor_expect_text(&fields[0], "expression plan key")?.to_owned(),
        lift,
        type_display: cbor_expect_text(&fields[2], "full expression type")?.to_owned(),
        heads: decode_nominal_heads(&fields[3], "expression result nominal heads")?,
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
    if items.iter().any(|item| {
        item.prologue_only
            && (item.verdict.kind != TurnKind::Decl
                || !item.verdict.binders.is_empty()
                || !item.source.is_empty())
    }) {
        return Err(CompileError::ExtractFailed(
            "CellOut CBOR: prologue-only item carries a declaration body or binder".into(),
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
    let fields = cbor_expect_array_len(value, 6, "cell item")?;
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
        prologue_only: cbor_expect_bool(&fields[5], "cell prologue-only flag")?,
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
    let temp = compiler_scratch_directory()?;
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

    let endpoint = bind_extract_cmd(&cmd)?;
    crate::paths::apply_admitted_build_products_dir(&mut cmd, &endpoint);
    let diagnostics = CompilerDiagnosticCapture::start(temp.path(), &cmd);
    let run = endpoint.execute(&cmd).map_err(map_notfound)?;
    diagnostics.completed(temp.path(), &cmd, run.success(), &run.output.stderr);
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
        refutable_binds, render_cell_compile_error, runtime_failure_advice,
        same_cell_value_collisions, AMBIGUOUS_TYPE_ADVICE, CELL_PURE_DISPATCH_ADVICE,
        LITERAL_ANNOTATION_ADVICE, SPLIT_SIGNATURE_ADVICE,
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
    /// only this shape is [`check_cell`]'s to fix.
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
    /// genuine error survives [`check_cell`]'s pinned
    /// retry does this text ever reach anyone (see that function's own
    /// tests for the common case, where the cell is admitted and no advice
    /// is shown at all).
    #[test]
    fn a_pure_dispatch_ambiguity_is_told_to_drop_pure_not_add_a_signature() {
        assert_eq!(
            ambiguous_type_advice(AMBIGUOUS_PURE_DISPATCH, "pure (1 :: Int)").as_deref(),
            Some(CELL_PURE_DISPATCH_ADVICE)
        );
        let rendered =
            render_cell_compile_error(&cell_error(AMBIGUOUS_PURE_DISPATCH), "pure (1 :: Int)");
        assert!(
            rendered.contains("Ambiguous type variable"),
            "GHC's own text must survive: {rendered}"
        );
        assert!(rendered.ends_with(CELL_PURE_DISPATCH_ADVICE), "{rendered}");
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
        assert!(
            ambiguous_type_advice(AMBIGUOUS_FIND_ELEM, "announce message = say message")
                .unwrap()
                .starts_with("`announce`'s type is ambiguous")
        );
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
        let rendered =
            render_cell_compile_error(&cell_error(message), "value = toJSON \"src/app.rs\"");
        assert!(rendered.contains("arising from the literal"), "{rendered}");
        assert!(rendered.ends_with(LITERAL_ANNOTATION_ADVICE), "{rendered}");
    }

    /// Each cell item compiles alone, so a signature whose equation went into
    /// the next item installs nothing and the next item reports the name as
    /// out of scope. The first item says so.
    #[test]
    fn a_signature_without_its_equation_says_they_share_one_item() {
        let message = "<cell>:1:1: error: [GHC-44432]\n    The type signature for \u{2018}summarize\u{2019} lacks an accompanying binding";
        let rendered =
            render_cell_compile_error(&cell_error(message), "summarize :: [Text] -> Text");
        assert!(
            rendered.contains("lacks an accompanying binding"),
            "{rendered}"
        );
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
        let rendered =
            render_cell_compile_error(&cell_error(AMBIGUOUS_REDECLARED_FIELD), "probe holder");
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
        let rendered =
            render_cell_compile_error(&cell_error(mismatch), "createWorktree currentCheckout");
        assert!(rendered.contains("WorktreeSeed"), "{rendered}");
        assert!(
            rendered.ends_with(&constructor_advice(mismatch).unwrap()),
            "{rendered}"
        );
    }

    #[test]
    fn a_type_that_takes_a_literal_is_left_to_its_instance() {
        // GitRef and BranchName have IsString, so a literal already works and
        // there is nothing to say.
        let git_ref = "<cell>:1:1: error:\n    Couldn't match expected type `GitRef' with actual type `[Char]'";
        assert_eq!(constructor_advice(git_ref), None);
    }

    /// `Cmd.stdout`/`Cmd.stderr` applied to the outcome-only `CommandResult` a
    /// completion event delivers, instead of the retained `RunResult` they
    /// actually take — the diagnostic named twice in the parked Astra flight.
    /// The hint is added BESIDE GHC's own text, never instead of it.
    #[test]
    fn command_result_stream_mismatch_adds_the_capture_hint() {
        let message = "Couldn't match expected type `RunResult' with actual type \
            `CommandResult'\n    In the first argument of `stdout', namely `event'";
        let rendered = render_cell_compile_error(&cell_error(message), "Cmd.stdout event");
        assert!(
            rendered.contains("CommandResult"),
            "GHC's own text must survive: {rendered}"
        );
        assert!(
            rendered.contains("Cmd.readStdout job"),
            "the recognized-error hint must be added beside GHC's text: {rendered}"
        );
    }

    /// `[Char]`/`String` vs `Text` at an argument of a stdlib function: the
    /// stdlib is Text-first, so the hint says the literal is already `Text`.
    #[test]
    fn string_text_mismatch_adds_the_stdlib_hint() {
        let message = "Couldn't match expected type `Text' with actual type `[Char]'";
        let rendered = render_cell_compile_error(&cell_error(message), "greet \"hi\"");
        assert!(
            rendered.contains("[Char]"),
            "GHC's own text must survive: {rendered}"
        );
        assert!(
            rendered.contains("Text-first"),
            "the recognized-error hint must be added beside GHC's text: {rendered}"
        );
    }

    /// `Text.pack` — a fresh-context child guessing the qualified alias
    /// sibling project code uses (`import qualified Data.Text as Text`)
    /// instead of the cell preamble's actual `T` — is GHC's ordinary
    /// not-in-scope shape for an unimported qualifier: the whole qualified
    /// name is reported missing, and the `Text` token is found inside it.
    #[test]
    fn qualified_text_alias_guess_adds_the_alias_hint() {
        let message = "Variable not in scope: Text.pack";
        let rendered = render_cell_compile_error(&cell_error(message), "Text.pack \"hi\"");
        assert!(
            rendered.contains("Text.pack"),
            "GHC's own text must survive: {rendered}"
        );
        assert!(
            rendered.contains("Data.Text is imported qualified as T"),
            "the recognized-error hint must be added beside GHC's text: {rendered}"
        );
        assert!(
            rendered.contains("use T.Text"),
            "the hint names the exact matched identifier: {rendered}"
        );
    }

    /// A bare `unlines`/`unpack`/… guess against the `T`-only preamble
    /// reports as an ordinary `Variable not in scope` diagnostic, not a
    /// qualified-name one — the same hint must still fire.
    #[test]
    fn bare_text_vocab_guess_adds_the_alias_hint() {
        let message = "Variable not in scope: unpack";
        let rendered = render_cell_compile_error(&cell_error(message), "unpack t");
        assert!(rendered.contains("use T.unpack"), "{rendered}");
    }

    /// The `Text` TYPE not in scope reports through GHC's type-constructor
    /// shape, not the value shape — still recognized.
    #[test]
    fn text_type_not_in_scope_adds_the_alias_hint() {
        let message = "Not in scope: type constructor or class \u{2018}Text\u{2019}";
        let rendered = render_cell_compile_error(&cell_error(message), "f :: Text -> Text");
        assert!(rendered.contains("use T.Text"), "{rendered}");
    }

    /// `unpackSomething` is not `unpack` — token matching must not false-fire
    /// on a name that merely contains a vocabulary word.
    #[test]
    fn a_name_merely_containing_a_text_vocab_word_adds_no_hint() {
        let message = "Variable not in scope: unpackSomething";
        let rendered = render_cell_compile_error(&cell_error(message), "unpackSomething t");
        assert!(
            !rendered.contains("Data.Text is imported qualified as T"),
            "{rendered}"
        );
    }

    /// Neither recognized pattern fires for an ordinary type mismatch that
    /// happens to share no vocabulary with either — the recognizer is narrow
    /// by construction, not a general GHC interpreter.
    #[test]
    fn an_unrelated_type_error_adds_no_stdlib_hint() {
        let message = "Couldn't match expected type `Int' with actual type `Bool'";
        let rendered = render_cell_compile_error(&cell_error(message), "1 == True");
        assert!(!rendered.contains("Text-first"), "{rendered}");
        assert!(!rendered.contains("Cmd.readStdout"), "{rendered}");
    }

    /// The exact text a cell gets today for `Right handle <- createWorktree …`
    /// when the worktree does not exist. Three layers of engine detail and a
    /// location in a generated wrapper; the reader wrote neither.
    const DO_BLOCK_FAILURE: &str = "prepared execution failed: Haskell exception raised: Pattern match failure in 'do' block at /tmp/nix-shell.FG8yXG/.tmpdKgy0W/Expr.hs:78:1-10";

    #[test]
    fn a_failed_pattern_bind_names_the_bind_and_says_to_inspect_the_value() {
        let cell =
            "Right tree <- createWorktree (fromRef (GitRef \"exomonad/dry8\") \"merge\")\ntree";
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
        assert!(
            advice.starts_with("a pattern bind, first `Just a` on line 2,"),
            "{advice}"
        );
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
    #[test]
    fn cell_observations_reject_old_wire_and_import_bearing_rows() {
        let array = CborValue::Array;
        let text = |value: &str| CborValue::Text(value.into());
        let payload = array(vec![
            array(vec![]),
            array(vec![]),
            text(""),
            array(vec![array(vec![]), array(vec![])]),
            array(vec![]),
        ]);
        let envelope = array(vec![text("TPCELLOBSERVATIONS"), 3.into(), payload.clone()]);
        let decode = |value: &CborValue| {
            let mut bytes = Vec::new();
            ciborium::ser::into_writer(value, &mut bytes).unwrap();
            decode_cell_out(&bytes, "", 0, "")
        };
        assert!(decode(&envelope).is_ok());
        assert!(decode(&payload).is_err());
        let mut old_version = envelope.clone();
        old_version.as_array_mut().unwrap()[1] = 2.into();
        assert!(decode(&old_version).is_err());
        let pin = array(vec![text("bind0_owned"), text("Int"), array(vec![])]);
        let expression = array(vec![
            text("expr0"),
            text("pure"),
            text("Int"),
            array(vec![]),
        ]);
        assert!(decode_checked_binder_pin(&pin).is_ok());
        assert!(decode_checked_expression_plan(&expression).is_ok());
        let mut old_pin = pin;
        old_pin
            .as_array_mut()
            .unwrap()
            .push(array(vec![text("Invented.Import")]));
        assert!(decode_checked_binder_pin(&old_pin).is_err());
        let mut old_expression = expression;
        old_expression
            .as_array_mut()
            .unwrap()
            .insert(2, text("opaque"));
        assert!(decode_checked_expression_plan(&old_expression).is_err());
    }

    #[test]
    #[serial_test::serial]
    fn runtime_cell_refuses_unknown_or_mismatched_deployment_before_worker_body() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let frontend = directory.path().join("frontend");
        let executed = directory.path().join("executed");
        std::fs::write(
            &frontend,
            format!(
                "#!/bin/sh\nprintf 'TPCID002{}{}'\nif IFS= read -r row; then touch '{}'; fi\n",
                "a".repeat(32),
                "b".repeat(32),
                executed.display(),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&frontend, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _frontend = TestEnvGuard::set("TIDEPOOL_EXTRACT", &frontend);
        let _daemon = TestEnvGuard::unset("TIDEPOOL_EXTRACT_DAEMON_SOCKET");
        let unknown = TestEnvGuard::unset("TIDEPOOL_COMPILER_DEPLOYMENT");
        let request = || CellCheckRequest {
            exact_context: None,
            session_id: None,
            cell_text: "let value = 42",
            template: include_str!("fixtures/checked-cell-template.hs"),
            include: &[],
            session_root: directory.path(),
            inject_modules: &[],
            compile_generation: 0,
            compile_view_evidence: "",
        };
        let error = check_cell(request()).unwrap_err().error;
        assert!(matches!(error, CompileError::ExtractFailed(ref message)
            if message.contains("no configured deployment authority")));
        assert!(!executed.exists());
        drop(unknown);
        let manifest = directory.path().join("deployment.json");
        let configuration = tidepool_toolchain::toolchain::CompilerDeploymentAuthority {
            schema: 1,
            producer_identity: [b'c'; 32],
            consumed_worker_identity: [b'b'; 32],
            frontend_path: frontend,
            worker_path: directory.path().join("worker"),
            ghc_libdir: directory.path().join("ghc"),
        };
        std::fs::write(&manifest, serde_json::to_vec(&configuration).unwrap()).unwrap();
        let _configuration = TestEnvGuard::set("TIDEPOOL_COMPILER_DEPLOYMENT", manifest);
        let error = check_cell(request()).unwrap_err().error;
        assert!(matches!(error, CompileError::ExtractFailed(ref message)
            if message.contains("bound producer differs from configured deployment")));
        assert!(
            !executed.exists(),
            "deployment mismatch executed compiler work"
        );
    }

    #[derive(Clone)]
    struct CheckedHomeOutput;

    impl crate::session::OutputSink for CheckedHomeOutput {
        fn drain(&self) -> Vec<String> {
            Vec::new()
        }

        fn snapshot(&self) -> Vec<String> {
            Vec::new()
        }
    }

    struct CheckedHomeCell {
        admission: Arc<crate::session::RuntimeCellAdmission>,
        checked: CellCheck,
        program: Arc<tidepool_toolchain::checked_cell::CellProgram>,
        templates: Vec<TurnTemplate>,
    }

    struct CheckedHomeFixture {
        root: TempDir,
        effects: tidepool_testing::effect_surface::TestEffectSurface,
        resident: crate::session::ResidentSession<frunk::HNil, CheckedHomeOutput>,
        execution: Arc<crate::session::PrivateExecutionAdmission>,
        home: Arc<tidepool_toolchain::checked_cell::CheckedValueArtifact>,
    }

    impl CheckedHomeFixture {
        fn new() -> Self {
            use crate::session::{ModuleEnv, PersistentSession, ResidentSession, SessionLib};
            use tidepool_codegen::scope::ScopeId;
            tidepool_testing::eval_harness::require_extract();
            let root = tempfile::tempdir().unwrap();
            std::fs::write(
                root.path().join("CheckedHomeValue.hs"),
                include_str!("fixtures/checked-home-value.hs"),
            )
            .unwrap();
            std::fs::write(
                root.path().join("CheckedHomeRelay.hs"),
                include_str!("fixtures/checked-home-relay.hs"),
            )
            .unwrap();
            let effects =
                tidepool_testing::effect_surface::TestEffectSurface::minimal(&[]).unwrap();
            let lib = SessionLib::open(
                tidepool_repr::SessionId(1005),
                root.path(),
                ModuleEnv::standalone_default(),
            )
            .unwrap()
            .with_validation_include(effects.include_paths().to_vec());
            let mut state = PersistentSession::new(Some(lib), 1024 * 1024);
            let public = state.mint_scope(ScopeId::ROOT).unwrap();
            let execution = Arc::new(state.begin_private_execution(public).unwrap());
            let mut resident =
                ResidentSession::from_persistent_for_test(frunk::HNil, CheckedHomeOutput, state);
            resident
                .set_run_context(crate::session::SessionRunContext {
                    lexical_scope: execution.private_scope(),
                    ..Default::default()
                })
                .unwrap();
            // Each fresh runtime owns its admission and completed native prefix.
            // The resulting immutable certificate is reused by every probe here.
            let cell = Self::check_in(&mut resident, &execution, &effects, root.path(),
                "let (home, homeNumber) = (CheckedHomeValue.homeValue, CheckedHomeValue.homeNumber)",
                &["qualified CheckedHomeValue"], None).unwrap();
            let home = Self::execute_in(&mut resident, cell)
                .into_iter()
                .next()
                .unwrap();
            assert!(home
                .certified_interface()
                .requirements()
                .iter()
                .any(|owner| owner.unit == "main" && owner.module == "CheckedHomeValue"));
            Self {
                root,
                effects,
                resident,
                execution,
                home,
            }
        }

        fn check_in(
            resident: &mut crate::session::ResidentSession<frunk::HNil, CheckedHomeOutput>,
            execution: &Arc<crate::session::PrivateExecutionAdmission>,
            effects: &tidepool_testing::effect_surface::TestEffectSurface,
            root: &Path,
            source: &str,
            imports: &[&str],
            first_include: Option<&Path>,
        ) -> Result<CheckedHomeCell, CompileError> {
            use crate::session::{
                resident_cell_check_template, resident_workbench_templates, SourceImports,
            };
            let view = resident.compile_view_for_execution(execution).unwrap();
            let imports = view.turn_imports(&SourceImports::from_specs(imports.iter().copied()));
            let template =
                resident_cell_check_template(effects.preamble(), effects.row(), &imports);
            let templates =
                resident_workbench_templates(effects.preamble(), effects.row(), &imports);
            let specification = CheckedCellSpecification {
                admission_digest: [0; 32],
                cell_source: source.into(),
                template_source: template.clone(),
                turn_templates: templates
                    .iter()
                    .map(|template| (template.kind.wire_name().into(), template.source.clone()))
                    .collect(),
                injected_modules: view.injected_module_names(),
                reserved_declaration_modules: Vec::new(),
            };
            let mut roots = effects.include_paths().to_vec();
            roots.insert(0, root.to_owned());
            if let Some(first) = first_include {
                roots.insert(0, first.to_owned());
            }
            let include_paths = view.include_paths(&roots);
            let plan = tidepool_toolchain::artifacts::parse_cell_plan(
                Arc::new(specification.clone()),
                &include_paths,
            )?;
            let admission = resident
                .admit_planned_cell_for_execution(
                    execution.clone(),
                    plan,
                    Arc::new(specification.clone()),
                    specification.specification_digest(),
                    [1; 32],
                    include_paths,
                    None,
                )
                .unwrap();
            let view = admission.view();
            let include = admission
                .include_paths()
                .iter()
                .map(PathBuf::as_path)
                .collect::<Vec<_>>();
            let (checked, program) = compile_cell_program_admitted(
                CellCheckRequest {
                    exact_context: view.exact_compile_context(),
                    session_id: Some(view.session()),
                    cell_text: source,
                    template: &template,
                    include: &include,
                    session_root: view.session_root(),
                    inject_modules: &specification.injected_modules,
                    compile_generation: admission.initial_value_generation().0,
                    compile_view_evidence: "",
                },
                admission.clone(),
                &templates,
            )
            .map_err(|failure| failure.error)?;
            Ok(CheckedHomeCell {
                admission,
                checked,
                program,
                templates,
            })
        }

        fn check(
            &mut self,
            source: &str,
            imports: &[&str],
            first_include: Option<&Path>,
        ) -> Result<CheckedHomeCell, CompileError> {
            Self::check_in(
                &mut self.resident,
                &self.execution,
                &self.effects,
                self.root.path(),
                source,
                imports,
                first_include,
            )
        }

        fn execute_in(
            resident: &mut crate::session::ResidentSession<frunk::HNil, CheckedHomeOutput>,
            cell: CheckedHomeCell,
        ) -> Vec<Arc<tidepool_toolchain::checked_cell::CheckedValueArtifact>> {
            let prefix = resident
                .begin_cell_program(cell.admission.clone(), cell.program)
                .unwrap()
                .expect("fixture must execute a nonempty checked program");
            let mut artifacts = Vec::new();
            for (index, observed) in cell.checked.items.iter().enumerate() {
                let item = cell.checked.checked_item(index).unwrap();
                let kind = item.kind();
                let expression_lift = item.expression_lift().unwrap();
                let declaration =
                    kind == tidepool_toolchain::checked_cell::CheckedItemKind::Declaration;
                let reservation = resident.admit_checked_item(prefix.clone(), item).unwrap();
                if declaration {
                    resident.adopt_checked_declaration(reservation).unwrap();
                    assert_eq!(prefix.snapshot().compiler_prefix().next_item(), index + 1);
                    continue;
                }
                let snapshot = reservation.snapshot();
                let view = snapshot.view();
                let injected = snapshot.compiler_prefix().injected_modules();
                let include = cell
                    .admission
                    .include_paths()
                    .iter()
                    .map(PathBuf::as_path)
                    .collect::<Vec<_>>();
                let TurnResult::Bind {
                    bound, compiled, ..
                } = run_checked_item(
                    TurnRequest {
                        exact_context: view.exact_compile_context(),
                        session_id: Some(view.session()),
                        turn_text: &observed.source,
                        templates: &cell.templates,
                        include: &include,
                        session_root: view.session_root(),
                        inject_modules: &injected,
                        gen: reservation.generation().0,
                        verdict: Some(observed.verdict.clone()),
                        target: None,
                        retained_imports: snapshot.admitted_retained_imports(),
                    },
                    reservation.clone(),
                )
                .unwrap()
                else {
                    panic!("the checked fixture must compile a native bind");
                };
                artifacts.push(
                    compiled
                        .certification
                        .as_ref()
                        .unwrap()
                        .checked_execution()
                        .unwrap()
                        .value_interface_certificate()
                        .unwrap(),
                );
                let outcome = match bound.as_slice() {
                    [binder]
                        if kind
                            == tidepool_toolchain::checked_cell::CheckedItemKind::Expression =>
                    {
                        resident.run_observation_with_sites(
                            compiled.code(),
                            binder,
                            reservation.generation(),
                            expression_lift
                                == Some(
                                    tidepool_toolchain::checked_cell::CheckedExpressionLift::Effectful,
                                ),
                        )
                    }
                    [binder] => resident.run_bind_with_sites(
                        &binder.name,
                        compiled.code(),
                        binder,
                        reservation.generation(),
                    ),
                    binders => resident.run_projected_bind_with_sites(
                        "checkedHome",
                        compiled.code(),
                        binders,
                        reservation.generation(),
                    ),
                }
                .unwrap();
                assert!(
                    matches!(
                        outcome,
                        crate::session::ResidentOutcome::Completed { .. }
                            | crate::session::ResidentOutcome::BindingsCommitted { .. }
                    ),
                    "native completion: {outcome:?}"
                );
                let visible = resident
                    .public_visibility_snapshot_in(
                        cell.admission.private_execution().unwrap().private_scope(),
                    )
                    .unwrap();
                for binder in &bound {
                    assert!(
                        visible
                            .bindings
                            .iter()
                            .any(|(name, id)| name == &binder.name
                                && *id == tidepool_repr::SessionVarId::from_extract(binder.var_id)),
                        "item {index} completed without its exact private binding {}: {outcome:?}",
                        binder.name,
                    );
                }
                assert_eq!(prefix.snapshot().compiler_prefix().next_item(), index + 1);
            }
            assert_eq!(
                artifacts.len(),
                cell.checked
                    .items
                    .iter()
                    .filter(|item| matches!(item.verdict.kind, TurnKind::Bind | TurnKind::Expr))
                    .count()
            );
            artifacts
        }

        fn execute(&mut self, cell: CheckedHomeCell) {
            Self::execute_in(&mut self.resident, cell);
        }

        fn assert_number(&mut self, name: &str, expected: i64) {
            let scope = self.resident.run_context().lexical_scope;
            let visible = self.resident.public_visibility_snapshot_in(scope).unwrap();
            let id = visible
                .bindings
                .iter()
                .find_map(|(binding, id)| (binding == name).then_some(*id))
                .unwrap_or_else(|| panic!("missing binding {name} in scope {scope:?}"));
            let custody = self
                .resident
                .retain_binding_custody_in(scope, name, id)
                .unwrap()
                .unwrap_or_else(|| {
                    panic!("binding {name} has no exact custody in scope {scope:?}")
                });
            assert_eq!(
                self.resident.render_retained_preview(&custody, 64),
                Some(expected.to_string()),
                "binding {name} in scope {scope:?}",
            );
        }

        fn publish_execution(&mut self, expected_names: &[&str]) {
            let intent = self
                .resident
                .freeze_private_execution(&self.execution)
                .unwrap();
            let mut expected = expected_names
                .iter()
                .map(|name| (*name).to_owned())
                .collect::<Vec<_>>();
            expected.sort();
            assert_eq!(intent.native_binding_names(), expected);
            let private = self
                .resident
                .public_visibility_snapshot_in(self.execution.private_scope())
                .unwrap();
            let private_winners = private
                .bindings
                .into_iter()
                .filter(|(name, _)| expected.contains(name))
                .collect::<Vec<_>>();
            assert_eq!(private_winners.len(), expected.len());
            let publication = self
                .resident
                .restage_ephemeral_execution_publication(intent)
                .unwrap();
            let ticket = match publication {
                crate::session::ExecutionPublication::Bindings(base) => base.stage().unwrap(),
                crate::session::ExecutionPublication::Declarations(base) => {
                    let crate::session::CertifiedDeclarationPublication::Accepted(accepted) =
                        base.certify().unwrap()
                    else {
                        panic!("checked history publication must be accepted");
                    };
                    accepted.stage().unwrap()
                }
            };
            assert_eq!(
                self.resident
                    .publish_staged_public_manifest(
                        ticket,
                        &crate::session::PublicationDecision::new()
                    )
                    .unwrap(),
                crate::session::PublicManifestCommit::Ephemeral
            );
            let public = self.execution.admitted_public().scope;
            let published = self.resident.public_visibility_snapshot_in(public).unwrap();
            assert_eq!(
                published
                    .bindings
                    .into_iter()
                    .filter(|(name, _)| expected.contains(name))
                    .collect::<Vec<_>>(),
                private_winners,
                "publication must preserve the exact settled native winners"
            );
            let mut context = self.resident.run_context();
            context.lexical_scope = public;
            self.resident.set_run_context(context).unwrap();
        }

        fn begin_next_execution(&mut self) {
            let public = self.execution.admitted_public().scope;
            self.execution = Arc::new(self.resident.begin_private_execution(public).unwrap());
            let mut context = self.resident.run_context();
            context.lexical_scope = self.execution.private_scope();
            self.resident.set_run_context(context).unwrap();
        }

        fn remove_sources(&self) {
            for name in ["CheckedHomeValue.hs", "CheckedHomeRelay.hs"] {
                std::fs::remove_file(self.root.path().join(name)).unwrap();
            }
        }

        fn assert_retained_original(&mut self) {
            let cell = self
                .check("let retainedObserved = homeNumber home", &[], None)
                .unwrap();
            self.execute(cell);
            self.assert_number("retainedObserved", 41);
        }
    }

    #[test]
    fn checked_expression_history_retains_one_original_capture_per_item() {
        use tidepool_toolchain::checked_cell::CheckedExpressionLift;
        let mut fixture = CheckedHomeFixture::new();
        fixture.publish_execution(&["home", "homeNumber"]);
        fixture.begin_next_execution();
        let initial = fixture.resident.val_gen().0 + 1;
        let cell = fixture
            .check(
                include_str!("fixtures/checked-expression-history.hs"),
                &[],
                None,
            )
            .unwrap();
        assert_eq!(cell.checked.items.len(), 4);
        assert_eq!(cell.program.items().len(), 4);
        assert_eq!(fixture.resident.val_gen().0, initial + 3);
        let reservation = cell.admission.plan_reservation().unwrap();
        let observations = reservation
            .items()
            .iter()
            .filter_map(|item| item.observation_name().map(str::to_owned))
            .collect::<Vec<_>>();
        assert_eq!(observations.len(), 2);
        for (index, (reserved, compiled)) in reservation
            .items()
            .iter()
            .zip(cell.program.items())
            .enumerate()
        {
            let generation = initial + index as u64;
            assert_eq!(reserved.value_generation().unwrap().0, generation);
            let native = compiled
                .native()
                .expect("every history item has its original native target");
            assert_eq!(native.generation(), generation);
            assert_eq!(
                native.value_interface_certificate().unwrap().owner(),
                tidepool_repr::SessionModule::val(tidepool_repr::Generation(generation))
            );
            assert_eq!(native.observation_name(), reserved.observation_name());
            let products = compiled.native_products().unwrap();
            assert!(products
                .certified_groups
                .iter()
                .all(|group| group.owner().module != "Tidepool.Inspection"));
            assert!(products
                .recovery_products
                .iter()
                .all(|product| product.owner().module != "Tidepool.Inspection"));
        }
        assert_eq!(
            cell.program.items()[1]
                .checked_item()
                .expression_lift()
                .unwrap(),
            Some(CheckedExpressionLift::Pure)
        );
        assert_eq!(
            cell.program.items()[2]
                .checked_item()
                .expression_lift()
                .unwrap(),
            Some(CheckedExpressionLift::Effectful)
        );
        fixture.execute(cell);
        let public = fixture.execution.admitted_public().scope;
        let visible = fixture
            .resident
            .public_visibility_snapshot_in(public)
            .unwrap();
        for name in [
            "historyStart",
            "historyEnd",
            &observations[0],
            &observations[1],
        ] {
            assert!(
                visible.bindings.iter().all(|(binding, _)| binding != name),
                "private history binding {name} became visible before cell publication"
            );
        }
        fixture.publish_execution(&[
            "historyStart",
            "historyEnd",
            &observations[0],
            &observations[1],
        ]);
        fixture.assert_number("historyEnd", 39);
        fixture.begin_next_execution();
        let followup = fixture.check(&format!(
            "let historyStart = (99 :: Int)\nlet originalPure = {} ()\nlet originalEffectful = {} ()\nlet originalEffectfulAgain = {} ()",
            observations[0], observations[1], observations[1],
        ), &[], None).unwrap();
        fixture.execute(followup);
        fixture.publish_execution(&[
            "historyStart",
            "originalPure",
            "originalEffectful",
            "originalEffectfulAgain",
        ]);
        fixture.assert_number("historyStart", 99);
        fixture.assert_number("originalPure", 37);
        fixture.assert_number("originalEffectful", 38);
        fixture.assert_number("originalEffectfulAgain", 38);
    }

    #[test]
    fn checked_explicit_home_import_executes_original_and_refuses_source_drift() {
        let mut fixture = CheckedHomeFixture::new();
        let cell = fixture
            .check(
                "import qualified CheckedHomeValue\nlet originalAgain = CheckedHomeValue.homeValue",
                &[],
                None,
            )
            .unwrap();
        assert_eq!(cell.checked.items.len(), 2);
        assert!(cell.checked.items[0].prologue_only);
        assert_eq!(
            cell.checked.checked_item(1).unwrap().binders(),
            &["originalAgain"]
        );
        fixture.execute(cell);
        let observe = fixture
            .check("let originalObserved = homeNumber originalAgain", &[], None)
            .unwrap();
        fixture.execute(observe);
        fixture.assert_number("originalObserved", 41);
        std::fs::write(
            fixture.root.path().join("CheckedHomeValue.hs"),
            include_str!("fixtures/checked-home-value.hs").replace("41", "42"),
        )
        .unwrap();
        let before = fixture
            .resident
            .public_visibility_snapshot_in(fixture.execution.private_scope())
            .unwrap();
        let Err(error) = fixture.check(
            "import qualified CheckedHomeValue\nlet changed = CheckedHomeValue.homeValue",
            &[],
            None,
        ) else {
            panic!("an authored current import accepted changed original source");
        };
        assert!(matches!(error, CompileError::InputRejected(_)), "{error:?}");
        assert_eq!(
            fixture
                .resident
                .public_visibility_snapshot_in(fixture.execution.private_scope())
                .unwrap(),
            before
        );
        fixture.assert_retained_original();
    }

    #[test]
    fn checked_explicit_helper_import_reproves_retained_home_dependency() {
        let mut fixture = CheckedHomeFixture::new();
        let cell = fixture
            .check(
                "import qualified CheckedHomeRelay\nlet relayAgain = CheckedHomeRelay.homeValue",
                &[],
                None,
            )
            .unwrap();
        assert_eq!(cell.checked.items.len(), 2);
        assert!(cell.checked.items[0].prologue_only);
        assert_eq!(
            cell.checked.checked_item(1).unwrap().binders(),
            &["relayAgain"]
        );
        fixture.execute(cell);
        let observe = fixture
            .check("let relayObserved = homeNumber relayAgain", &[], None)
            .unwrap();
        fixture.execute(observe);
        fixture.assert_number("relayObserved", 41);
        std::fs::write(
            fixture.root.path().join("CheckedHomeValue.hs"),
            include_str!("fixtures/checked-home-value.hs").replace("41", "42"),
        )
        .unwrap();
        let before = fixture
            .resident
            .public_visibility_snapshot_in(fixture.execution.private_scope())
            .unwrap();
        let Err(error) = fixture.check(
            "import qualified CheckedHomeRelay\nlet changedRelay = CheckedHomeRelay.homeValue",
            &[],
            None,
        ) else {
            panic!("a fresh helper accepted changed retained source");
        };
        assert!(matches!(error, CompileError::InputRejected(_)), "{error:?}");
        assert_eq!(
            fixture
                .resident
                .public_visibility_snapshot_in(fixture.execution.private_scope())
                .unwrap(),
            before
        );
        fixture.assert_retained_original();
    }

    #[test]
    fn checked_explicit_home_import_refuses_ordered_path_shadow() {
        let mut fixture = CheckedHomeFixture::new();
        let shadow = fixture.root.path().join("shadow");
        std::fs::create_dir(&shadow).unwrap();
        std::fs::write(
            shadow.join("CheckedHomeValue.hs"),
            include_str!("fixtures/checked-home-value.hs"),
        )
        .unwrap();
        let before = fixture
            .resident
            .public_visibility_snapshot_in(fixture.execution.private_scope())
            .unwrap();
        let Err(error) = fixture.check(
            "import qualified CheckedHomeValue\nlet shadowed = CheckedHomeValue.homeValue",
            &[],
            Some(&shadow),
        ) else {
            panic!("current import accepted identical bytes from another ordered source path");
        };
        assert!(matches!(error, CompileError::InputRejected(_)), "{error:?}");
        assert_eq!(
            fixture
                .resident
                .public_visibility_snapshot_in(fixture.execution.private_scope())
                .unwrap(),
            before
        );
        assert!(fixture.root.path().join("CheckedHomeValue.hs").is_file());
        fixture.assert_retained_original();
    }

    #[test]
    fn checked_captured_home_references_execute_original_after_source_drift() {
        let mut fixture = CheckedHomeFixture::new();
        std::fs::write(
            fixture.root.path().join("CheckedHomeValue.hs"),
            include_str!("fixtures/checked-home-value.hs").replace("41", "42"),
        )
        .unwrap();
        // Generated template imports expose captured Names; only an import
        // in the submitted source prologue requests current source selection.
        for (source, imports, value) in [
            (
                "let capturedQualified = homeNumber CheckedHomeValue.homeValue",
                "qualified CheckedHomeValue",
                "capturedQualified",
            ),
            (
                "let capturedUnqualified = homeNumber homeValue",
                "CheckedHomeValue (homeValue)",
                "capturedUnqualified",
            ),
        ] {
            let cell = fixture.check(source, &[imports], None).unwrap();
            assert_eq!(cell.checked.items.len(), 1);
            fixture.execute(cell);
            fixture.assert_number(value, 41);
        }
        fixture.assert_retained_original();
    }

    #[test]
    fn checked_retained_home_value_executes_after_original_source_removal() {
        let mut fixture = CheckedHomeFixture::new();
        fixture.remove_sources();
        fixture.assert_retained_original();
        assert!(!fixture.root.path().join("CheckedHomeValue.hs").exists());
    }

    #[test]
    fn checked_later_item_reuses_retained_home_type_and_native_prefix_without_source() {
        let mut fixture = CheckedHomeFixture::new();
        fixture.remove_sources();
        let cell = fixture
            .check(
                "let alias = home\nlet aliasObserved = homeNumber alias",
                &[],
                None,
            )
            .unwrap();
        assert_eq!(cell.checked.items.len(), 2);
        assert!(cell.checked.checked_item(0).unwrap().signatures()[0]
            .names()
            .iter()
            .any(|name| name.unit() == "main"
                && name.module() == "CheckedHomeValue"
                && name.occurrence() == "HomeValue"));
        fixture.execute(cell);
        fixture.assert_number("aliasObserved", 41);
        assert!(!fixture.root.path().join("CheckedHomeValue.hs").exists());
    }

    #[test]
    fn checked_retained_home_certificate_refuses_substituted_interface() {
        let fixture = CheckedHomeFixture::new();
        let scratch = tempfile::tempdir().unwrap();
        let endpoint = extract_cmd().unwrap().bind().unwrap();
        let specification = CheckedCellSpecification {
            admission_digest: [3; 32],
            cell_source: "let alias = home".into(),
            template_source: "module Next where".into(),
            turn_templates: vec![],
            injected_modules: vec![fixture.home.owner().module_name()],
            reserved_declaration_modules: vec![],
        };
        let error = ModuleCandidateOffer::select_checked_cell(
            endpoint.identity().producer_bytes(),
            fixture.effects.include_paths(),
            scratch.path(),
            None,
            specification,
            tidepool_toolchain::artifacts::CheckedCellPurpose::Authored,
            vec![(fixture.home.owner(), Arc::from(&b"changed interface"[..]))],
            std::slice::from_ref(&fixture.home),
            &[],
        )
        .err()
        .expect("another interface cannot borrow the retained certificate");
        assert!(
            matches!(&error, CompileError::ExtractFailed(detail)
            if detail == "checked cell: retained value certificate differs from selected interface bytes"),
            "{error:?}"
        );
    }

    fn compile_public_checked_offer(
        admission: &Arc<crate::session::RuntimeCellAdmission>,
        specification: CheckedCellSpecification,
    ) -> (TempDir, ExactCheckedItem) {
        compile_public_checked_offer_with_inputs(
            admission,
            specification,
            admission.include_paths(),
            admission.include_paths(),
        )
        .unwrap()
    }

    fn compile_public_checked_offer_with_inputs(
        admission: &Arc<crate::session::RuntimeCellAdmission>,
        mut specification: CheckedCellSpecification,
        offered_include: &[PathBuf],
        worker_include: &[PathBuf],
    ) -> Result<(TempDir, ExactCheckedItem), CompileError> {
        let temp = TempDir::new()?;
        let source = temp.path().join("cell.txt");
        let template = temp.path().join("CellCheckTemplate.hs");
        std::fs::write(&source, &specification.cell_source)?;
        std::fs::write(&template, &specification.template_source)?;
        specification.admission_digest = admission.digest();
        let include_paths = worker_include
            .iter()
            .map(PathBuf::as_path)
            .collect::<Vec<_>>();
        let mut cmd = extract_cmd()?;
        cmd.input(&source)
            .cell()
            .cell_template(&template)
            .cell_out(temp.path().join("cell.cbor"))
            .output_dir(temp.path())
            .includes(&include_paths)
            .session_incarnation(admission.view().session().0.to_string());
        let endpoint = cmd.bind().map_err(map_notfound)?;
        let offer = ModuleCandidateOffer::select_checked_cell(
            endpoint.identity().producer_bytes(),
            offered_include,
            temp.path(),
            admission.view().exact_compile_context(),
            specification,
            tidepool_toolchain::artifacts::CheckedCellPurpose::Authored,
            Vec::new(),
            &[],
            &admission.retained_declaration_projections(),
        )?;
        cmd.session_root(offer.checked_value_root().unwrap())
            .session_artifacts(offer.exact_scope_path().unwrap());
        crate::paths::apply_build_products_dir(&mut cmd, &endpoint);
        let run = endpoint.execute(&cmd).map_err(map_notfound)?;
        if let Err(error) = crate::diag::decode_extract_result(
            run.success(),
            &run.output.stdout,
            &run.output.stderr,
        ) {
            return Err(offer.retain_failure(temp.path(), &cmd, &run.output.stderr, error));
        }
        let cell = offer
            .admit_checked_cell(temp.path())
            .map_err(|error| offer.retain_failure(temp.path(), &cmd, &run.output.stderr, error))?;
        Ok((temp, cell.item(0)?))
    }

    #[test]
    fn dependency_load_errors_keep_source_spans_and_allow_a_valid_retry() {
        use crate::session::{ModuleEnv, PersistentSession, SessionLib};
        use tidepool_codegen::scope::ScopeId;
        use tidepool_repr::SessionId;
        tidepool_testing::eval_harness::require_extract();
        let root = tempfile::tempdir().unwrap();
        let dependency = root.path().join("RootChoice.hs");
        std::fs::write(
            &dependency,
            include_str!("fixtures/checked-search-invalid.hs"),
        )
        .unwrap();
        let specification = CheckedCellSpecification {
            admission_digest: [0; 32],
            cell_source: "let value = RootChoice.value".into(),
            template_source: include_str!("fixtures/checked-cell-template.hs").replace(
                "{{CELL_IMPORTS}}",
                "import qualified RootChoice\n{{CELL_IMPORTS}}",
            ),
            turn_templates: Vec::new(),
            injected_modules: Vec::new(),
            reserved_declaration_modules: Vec::new(),
        };
        let include = vec![root.path().to_path_buf()];
        let lib = SessionLib::open(
            SessionId(1010),
            root.path(),
            ModuleEnv::standalone_default(),
        )
        .unwrap();
        let mut state = PersistentSession::new(Some(lib), 1024 * 1024);
        let public = state.mint_scope(ScopeId::ROOT).unwrap();
        let execution = Arc::new(state.begin_private_execution(public).unwrap());
        let admission = state
            .admit_cell_for_execution(
                execution,
                0,
                Arc::new(specification.clone()),
                specification.specification_digest(),
                [1; 32],
                include.clone(),
            )
            .unwrap();
        let ordinary = || {
            let temp = TempDir::new().unwrap();
            let source = temp.path().join("cell.txt");
            let template = temp.path().join("template.hs");
            std::fs::write(&source, &specification.cell_source).unwrap();
            std::fs::write(&template, &specification.template_source).unwrap();
            let mut cmd = extract_cmd().unwrap();
            cmd.input(&source)
                .cell()
                .cell_template(&template)
                .cell_out(temp.path().join("cell.cbor"))
                .output_dir(temp.path())
                .includes(&[root.path()]);
            let endpoint = cmd.bind().unwrap();
            let run = endpoint.execute(&cmd).unwrap();
            crate::diag::decode_extract_result(
                run.success(),
                &run.output.stdout,
                &run.output.stderr,
            )
        };
        let assert_source_failure = |failure: CompileError| {
            assert_eq!(
                crate::failclass::classify_compile(&failure).class,
                crate::failclass::FailureClass::UserHaskell
            );
            let CompileError::Diagnostics(diagnostics) = failure else {
                panic!("a dependency type error must remain a structured source failure")
            };
            assert!(
                diagnostics.iter().any(|diagnostic| {
                    diagnostic.severity == crate::diag::DiagnosticSeverity::Error
                        && diagnostic.span.as_ref().is_some_and(|span| {
                            Path::new(&span.file) == dependency && span.start_line == 3
                        })
                }),
                "the original dependency source span must survive the load barrier"
            );
        };
        assert_source_failure(ordinary().unwrap_err());
        assert_source_failure(
            compile_public_checked_offer_with_inputs(
                &admission,
                specification.clone(),
                &include,
                &include,
            )
            .unwrap_err(),
        );
        std::fs::write(
            &dependency,
            include_str!("fixtures/checked-search-original.hs"),
        )
        .unwrap();
        ordinary().unwrap();
        let (_artifacts, first) = compile_public_checked_offer(&admission, specification);
        state.begin_checked_prefix(admission, first).unwrap();
    }

    #[test]
    fn substituted_search_inputs_refuse_before_compilation_or_prefix_claim() {
        use crate::session::{ModuleEnv, PersistentSession, SessionLib};
        use tidepool_codegen::scope::ScopeId;
        use tidepool_repr::SessionId;
        tidepool_testing::eval_harness::require_extract();
        let root = tempfile::tempdir().unwrap();
        let original_root = root.path().join("original");
        let alternate_root = root.path().join("alternate");
        std::fs::create_dir(&original_root).unwrap();
        std::fs::create_dir(&alternate_root).unwrap();
        std::fs::write(
            original_root.join("RootChoice.hs"),
            include_str!("fixtures/checked-search-original.hs"),
        )
        .unwrap();
        std::fs::write(
            alternate_root.join("RootChoice.hs"),
            include_str!("fixtures/checked-search-alternate.hs"),
        )
        .unwrap();
        let lib = SessionLib::open(
            SessionId(1001),
            root.path(),
            ModuleEnv::standalone_default(),
        )
        .unwrap();
        let mut state = PersistentSession::new(Some(lib), 1024 * 1024);
        let public = state.mint_scope(ScopeId::ROOT).unwrap();
        let execution = Arc::new(state.begin_private_execution(public).unwrap());
        let specification = CheckedCellSpecification {
            admission_digest: [0; 32],
            cell_source: "let value = RootChoice.value".into(),
            template_source: include_str!("fixtures/checked-cell-template.hs").replace(
                "{{CELL_IMPORTS}}",
                "import qualified RootChoice\n{{CELL_IMPORTS}}",
            ),
            turn_templates: Vec::new(),
            injected_modules: Vec::new(),
            reserved_declaration_modules: Vec::new(),
        };
        let original_include = vec![original_root.clone(), root.path().to_path_buf()];
        let alternate_include = vec![alternate_root.clone(), root.path().to_path_buf()];
        let admission = state
            .admit_cell_for_execution(
                execution,
                0,
                Arc::new(specification.clone()),
                specification.specification_digest(),
                [1; 32],
                original_include.clone(),
            )
            .unwrap();
        // The runtime front door refuses different roots before creating a worker offer.
        let alternate_refs = alternate_include
            .iter()
            .map(PathBuf::as_path)
            .collect::<Vec<_>>();
        assert!(check_cell_admitted(
            CellCheckRequest {
                exact_context: admission.view().exact_compile_context(),
                session_id: Some(admission.view().session()),
                cell_text: &specification.cell_source,
                template: &specification.template_source,
                include: &alternate_refs,
                session_root: admission.view().session_root(),
                inject_modules: &[],
                compile_generation: admission.initial_value_generation().0,
                compile_view_evidence: "",
            },
            admission.clone(),
            &[]
        )
        .is_err());
        // A public offer cannot label the original roots while invoking different roots.
        let failure = compile_public_checked_offer_with_inputs(
            &admission,
            specification.clone(),
            &original_include,
            &alternate_include,
        )
        .unwrap_err();
        assert!(
            matches!(failure, CompileError::InputRejected(_)),
            "{failure}"
        );
        // A faithfully labelled alternate compilation can seal, but cannot claim this admission.
        let (alternate_artifacts, alternate) = compile_public_checked_offer_with_inputs(
            &admission,
            specification.clone(),
            &alternate_include,
            &alternate_include,
        )
        .unwrap();
        let receipts = std::fs::read_dir(alternate_artifacts.path().join(".exact-compilations"))
            .unwrap()
            .map(|entry| entry.unwrap().path().join("receipt.cbor"))
            .collect::<Vec<_>>();
        assert!(!receipts.is_empty());
        let selected = alternate_root.join("RootChoice.hs");
        assert!(receipts.iter().any(|receipt| {
            let value: ciborium::value::Value =
                ciborium::de::from_reader(std::fs::read(receipt).unwrap().as_slice()).unwrap();
            let evidence = value.as_array().unwrap()[7].as_text().unwrap();
            evidence.contains(selected.to_str().unwrap())
        }));
        assert_eq!(
            alternate.specification_digest(),
            admission.specification_digest()
        );
        assert!(state
            .begin_checked_prefix(admission.clone(), alternate)
            .is_err());
        let (_artifacts, legitimate) = compile_public_checked_offer(&admission, specification);
        assert_eq!(legitimate.include_paths(), admission.include_paths());
        state
            .begin_checked_prefix(admission, legitimate)
            .expect("refused alternate roots must leave the original admission unclaimed");
    }

    #[test]
    fn substituted_checked_specification_refuses_before_claiming_prefix() {
        use crate::session::{ModuleEnv, PersistentSession, SessionLib};
        use tidepool_codegen::scope::ScopeId;
        use tidepool_repr::SessionId;
        tidepool_testing::eval_harness::require_extract();
        let root = tempfile::tempdir().unwrap();
        let lib =
            SessionLib::open(SessionId(999), root.path(), ModuleEnv::standalone_default()).unwrap();
        let mut state = PersistentSession::new(Some(lib), 1024 * 1024);
        let public = state.mint_scope(ScopeId::ROOT).unwrap();
        let execution = Arc::new(state.begin_private_execution(public).unwrap());
        let specification = CheckedCellSpecification {
            admission_digest: [0; 32],
            cell_source: "let value = (42 :: Int)".into(),
            template_source: include_str!("fixtures/checked-cell-template.hs").into(),
            turn_templates: Vec::new(),
            injected_modules: Vec::new(),
            reserved_declaration_modules: Vec::new(),
        };
        let admission = state
            .admit_cell_for_execution(
                execution,
                0,
                Arc::new(specification.clone()),
                specification.specification_digest(),
                [1; 32],
                vec![root.path().to_path_buf()],
            )
            .unwrap();
        let mut alternate = specification.clone();
        alternate.cell_source = "let value = (43 :: Int)".into();
        let (_alternate_artifacts, alternate) = compile_public_checked_offer(&admission, alternate);
        assert_eq!(alternate.admission_digest(), admission.digest());
        assert_ne!(
            alternate.specification_digest(),
            admission.specification_digest()
        );
        assert!(state
            .begin_checked_prefix(admission.clone(), alternate)
            .is_err());
        let (_legitimate_artifacts, legitimate) =
            compile_public_checked_offer(&admission, specification);
        assert_eq!(
            legitimate.specification_digest(),
            admission.specification_digest()
        );
        let prefix = state
            .begin_checked_prefix(admission, legitimate)
            .expect("refused alternate specification must not claim the one-shot prefix");
        assert_eq!(prefix.snapshot().compiler_prefix().next_item(), 0);
    }

    #[test]
    fn substituted_reserved_original_refuses_before_claiming_prefix() {
        use crate::session::{ModuleEnv, PersistentSession, SessionLib};
        use tidepool_codegen::scope::ScopeId;
        use tidepool_repr::{SessionId, SessionModule};
        tidepool_testing::eval_harness::require_extract();
        let root = tempfile::tempdir().unwrap();
        let lib = SessionLib::open(
            SessionId(1000),
            root.path(),
            ModuleEnv::standalone_default(),
        )
        .unwrap();
        let mut state = PersistentSession::new(Some(lib), 1024 * 1024);
        let public = state.mint_scope(ScopeId::ROOT).unwrap();
        let execution = Arc::new(state.begin_private_execution(public).unwrap());
        let mut specification = CheckedCellSpecification {
            admission_digest: [0; 32],
            cell_source: "originalValue = (42 :: Int)".into(),
            template_source: include_str!("fixtures/checked-cell-template.hs").into(),
            turn_templates: Vec::new(),
            injected_modules: Vec::new(),
            reserved_declaration_modules: Vec::new(),
        };
        let admission = state
            .admit_cell_for_execution(
                execution,
                1,
                Arc::new(specification.clone()),
                specification.specification_digest(),
                [1; 32],
                vec![root.path().to_path_buf()],
            )
            .unwrap();
        let reserved = admission.reserved_generations()[0];
        specification.reserved_declaration_modules =
            vec![SessionModule::lib(reserved).module_name()];
        let mut alternate = specification.clone();
        alternate.reserved_declaration_modules =
            vec![SessionModule::lib(tidepool_repr::Generation(reserved.0 + 1)).module_name()];
        let (_alternate_artifacts, alternate) = compile_public_checked_offer(&admission, alternate);
        assert_eq!(alternate.admission_digest(), admission.digest());
        assert_eq!(
            alternate.specification_digest(),
            admission.specification_digest()
        );
        assert_ne!(
            alternate.reserved_declaration_modules(),
            specification.reserved_declaration_modules
        );
        assert!(state
            .begin_checked_prefix(admission.clone(), alternate)
            .is_err());
        let (_legitimate_artifacts, legitimate) =
            compile_public_checked_offer(&admission, specification);
        assert_eq!(
            legitimate.reserved_declaration_modules(),
            [SessionModule::lib(reserved).module_name()]
        );
        let prefix = state
            .begin_checked_prefix(admission, legitimate)
            .expect("refused alternate original owner must not claim the one-shot prefix");
        assert_eq!(prefix.snapshot().compiler_prefix().next_item(), 0);
    }

    #[test]
    fn empty_checked_context_reuses_immutable_support_and_invalidates_changed_source() {
        use crate::session::{
            resident_cell_check_template, resident_workbench_templates, ModuleEnv,
            PersistentSession, SessionLib,
        };
        use tidepool_codegen::scope::ScopeId;
        use tidepool_repr::SessionId;
        use tidepool_testing::effect_surface::TestEffectSurface;
        use tidepool_toolchain::certified_products::ProductOrigin;
        tidepool_testing::eval_harness::require_extract();
        let _daemon = TestEnvGuard::unset("TIDEPOOL_EXTRACT_DAEMON_SOCKET");
        let cache = tempfile::tempdir().unwrap();
        let _cache = TestEnvGuard::set("TIDEPOOL_COMPILE_CACHE_DIR", cache.path());
        let root = tempfile::tempdir().unwrap();
        let support = root.path().join("CheckedTiny.hs");
        let support_source = include_str!("fixtures/checked-tiny-support.hs");
        std::fs::write(&support, support_source).unwrap();
        let effects = TestEffectSurface::minimal(&[]).unwrap();
        let lib = SessionLib::open(SessionId(997), root.path(), ModuleEnv::standalone_default())
            .unwrap()
            .with_validation_include(effects.include_paths().to_vec());
        let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
        let public = session.mint_scope(ScopeId::ROOT).unwrap();
        for (index, expected) in [
            ProductOrigin::Fresh,
            ProductOrigin::Cached,
            ProductOrigin::Fresh,
        ]
        .into_iter()
        .enumerate()
        {
            if index == 2 {
                std::fs::write(&support, support_source.replace("41", "42")).unwrap();
            }
            let execution = Arc::new(session.begin_private_execution(public).unwrap());
            let view = execution.view();
            let imports = view.turn_imports(&crate::session::SourceImports::from_specs([
                "qualified CheckedTiny",
            ]));
            let template =
                resident_cell_check_template(effects.preamble(), effects.row(), &imports);
            let templates =
                resident_workbench_templates(effects.preamble(), effects.row(), &imports);
            let source = "let tiny = CheckedTiny.tinyValue";
            let specification = CheckedCellSpecification {
                admission_digest: [0; 32],
                cell_source: source.into(),
                template_source: template.clone(),
                turn_templates: templates
                    .iter()
                    .map(|template| (template.kind.wire_name().into(), template.source.clone()))
                    .collect(),
                injected_modules: view.injected_module_names(),
                reserved_declaration_modules: Vec::new(),
            };
            let mut roots = effects.include_paths().to_vec();
            roots.insert(0, root.path().to_owned());
            let admitted_include = view.include_paths(&roots);
            let admission = session
                .admit_cell_for_execution(
                    execution,
                    0,
                    Arc::new(specification.clone()),
                    specification.specification_digest(),
                    [1; 32],
                    admitted_include,
                )
                .unwrap();
            let view = admission.view();
            let include = admission.include_paths();
            let include = include.iter().map(PathBuf::as_path).collect::<Vec<_>>();
            let injected = view.injected_module_names();
            let started = std::time::Instant::now();
            let checked = check_cell_admitted(
                CellCheckRequest {
                    exact_context: view.exact_compile_context(),
                    session_id: Some(view.session()),
                    cell_text: source,
                    template: &template,
                    include: &include,
                    session_root: view.session_root(),
                    inject_modules: &injected,
                    compile_generation: admission.initial_value_generation().0,
                    compile_view_evidence: "",
                },
                admission.clone(),
                &templates,
            )
            .unwrap();
            let item = checked.checked_item(0).unwrap();
            let prefix = session
                .begin_checked_prefix(admission.clone(), item.clone())
                .unwrap();
            let item_admission = session.admit_checked_item(prefix, item).unwrap();
            let TurnResult::Bind { compiled, .. } = run_checked_item(
                TurnRequest {
                    exact_context: view.exact_compile_context(),
                    session_id: Some(view.session()),
                    turn_text: source,
                    templates: &templates,
                    include: &include,
                    session_root: view.session_root(),
                    inject_modules: &injected,
                    gen: admission.initial_value_generation().0,
                    verdict: Some(checked.items[0].verdict.clone()),
                    target: None,
                    retained_imports: &[],
                },
                item_admission,
            )
            .unwrap() else {
                panic!("the runtime-admitted item must be a bind");
            };
            let groups = &compiled.certification.unwrap().groups;
            let support_groups = groups
                .iter()
                .filter(|group| group.owner().module == "CheckedTiny")
                .collect::<Vec<_>>();
            assert!(
                !support_groups.is_empty(),
                "support group was not certified"
            );
            assert!(support_groups
                .iter()
                .all(|group| group.origin() == expected));
            eprintln!(
                "checked-tiny index={index} elapsed_ms={} fresh_groups={} cached_groups={}",
                started.elapsed().as_millis(),
                groups
                    .iter()
                    .filter(|group| group.origin() == ProductOrigin::Fresh)
                    .count(),
                groups
                    .iter()
                    .filter(|group| group.origin() == ProductOrigin::Cached)
                    .count()
            );
        }
    }

    #[test]
    fn unsupported_declaration_order_is_a_source_contract_rejection() {
        use crate::session::{
            resident_cell_check_template, ModuleEnv, PersistentSession, SessionLib,
        };
        use tidepool_codegen::scope::ScopeId;
        use tidepool_repr::SessionId;
        use tidepool_testing::effect_surface::TestEffectSurface;
        tidepool_testing::eval_harness::require_extract();
        let root = tempfile::tempdir().unwrap();
        let effects = TestEffectSurface::minimal(&[]).unwrap();
        let lib = SessionLib::open(
            SessionId(1011),
            root.path(),
            ModuleEnv::standalone_default(),
        )
        .unwrap()
        .with_validation_include(effects.include_paths().to_vec());
        let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
        let public = session.mint_scope(ScopeId::ROOT).unwrap();
        let execution = Arc::new(session.begin_private_execution(public).unwrap());
        let view = execution.view();
        let template = resident_cell_check_template(
            effects.preamble(),
            effects.row(),
            &view.turn_imports(&crate::session::SourceImports::new()),
        );
        let source = include_str!("fixtures/checked-interleaved-declaration.hs");
        let specification = CheckedCellSpecification {
            admission_digest: [0; 32],
            cell_source: source.into(),
            template_source: template.clone(),
            turn_templates: Vec::new(),
            injected_modules: view.injected_module_names(),
            reserved_declaration_modules: Vec::new(),
        };
        let include = view.include_paths(effects.include_paths());
        let admission = session
            .admit_cell_for_execution(
                execution,
                1,
                Arc::new(specification.clone()),
                specification.specification_digest(),
                [1; 32],
                include,
            )
            .unwrap();
        let view = admission.view();
        let include = view.include_paths(effects.include_paths());
        let include = include.iter().map(PathBuf::as_path).collect::<Vec<_>>();
        let outcome = check_cell_admitted(
            CellCheckRequest {
                exact_context: view.exact_compile_context(),
                session_id: Some(view.session()),
                cell_text: source,
                template: &template,
                include: &include,
                session_root: view.session_root(),
                inject_modules: &view.injected_module_names(),
                compile_generation: admission.initial_value_generation().0,
                compile_view_evidence: "",
            },
            admission.clone(),
            &[],
        );
        let failure = match outcome {
            Err(failure) => failure,
            Ok(_) => panic!("interleaved declaration order was admitted before ordered planning"),
        };
        assert!(matches!(failure.error, CompileError::Diagnostics(_)));
        assert_eq!(
            crate::failclass::classify_compile(&failure.error).class,
            crate::failclass::FailureClass::UserHaskell
        );
    }

    #[test]
    fn checked_declaration_requires_private_custody_and_releases_after_adoption() {
        use crate::session::{
            resident_cell_check_template, ModuleEnv, PersistentSession, SessionLib, SourceImports,
        };
        use tidepool_codegen::scope::ScopeId;
        use tidepool_repr::{Generation, SessionId};
        use tidepool_testing::effect_surface::TestEffectSurface;
        tidepool_testing::eval_harness::require_extract();
        let effects = TestEffectSurface::minimal(&[]).unwrap();
        for private in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let lib = SessionLib::open(
                SessionId(1016),
                root.path(),
                ModuleEnv::standalone_default(),
            )
            .unwrap()
            .with_validation_include(effects.include_paths().to_vec());
            let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
            let lease = session.retain_lexical_scope(ScopeId::ROOT).unwrap();
            let public = lease.scope();
            let execution =
                private.then(|| Arc::new(session.begin_private_execution(public).unwrap()));
            let scope = execution
                .as_ref()
                .map_or(public, |owner| owner.private_scope());
            let view = session.compile_view_in(scope).unwrap();
            let declaration_module = session.next_lib_module().unwrap();
            assert_eq!(declaration_module.gen(), Generation(1));
            let preamble = effects.preamble().replace(
                "module Expr where",
                &format!("module {} where", declaration_module.module_name()),
            );
            let template = resident_cell_check_template(
                &preamble,
                effects.row(),
                &view.turn_imports(&SourceImports::new()),
            );
            let source = "data RetainedLeaseDeclaration = RetainedLeaseDeclaration";
            let specification = CheckedCellSpecification {
                admission_digest: [0; 32],
                cell_source: source.into(),
                template_source: template.clone(),
                turn_templates: Vec::new(),
                injected_modules: view.injected_module_names(),
                reserved_declaration_modules: Vec::new(),
            };
            let include_paths = view.include_paths(effects.include_paths());
            let admission = match &execution {
                Some(execution) => session.admit_cell_for_execution(
                    execution.clone(),
                    1,
                    Arc::new(specification.clone()),
                    specification.specification_digest(),
                    [1; 32],
                    include_paths,
                ),
                None => session.admit_cell_in(
                    scope,
                    1,
                    Arc::new(specification.clone()),
                    specification.specification_digest(),
                    [1; 32],
                    include_paths,
                ),
            }
            .unwrap();
            let view = admission.view();
            let include = admission
                .include_paths()
                .iter()
                .map(PathBuf::as_path)
                .collect::<Vec<_>>();
            let injected = view.injected_module_names();
            let checked = check_cell_admitted(
                CellCheckRequest {
                    exact_context: view.exact_compile_context(),
                    session_id: Some(view.session()),
                    cell_text: source,
                    template: &template,
                    include: &include,
                    session_root: view.session_root(),
                    inject_modules: &injected,
                    compile_generation: admission.initial_value_generation().0,
                    compile_view_evidence: "",
                },
                admission.clone(),
                &[],
            );
            let original = session.public_visibility_snapshot_in(scope).unwrap();
            assert_eq!(original.declaration_tip, Generation(0));
            assert_eq!(original.epoch, 0);
            assert!(session
                .lib()
                .log
                .certified_authored_at(Generation(1))
                .is_none());
            if !private {
                let failure = checked
                    .err()
                    .expect("raw admission cannot check declarations");
                let CompileError::ExtractFailed(message) = failure.error else {
                    panic!("raw admission must be refused at the execution-owner boundary");
                };
                assert_eq!(
                    message,
                    "checked execution requires its owning private or native setup admission"
                );
                assert!(admission.private_execution().is_none());
                assert_eq!(session.public_visibility_snapshot_in(scope), Some(original));
                assert!(session.lib().log.is_reserved(Generation(1)));
                continue;
            }
            let checked = checked.unwrap();
            assert_eq!(checked.items.len(), 1);
            let item = checked.checked_item(0).unwrap();
            assert!(item.planned_declaration().is_some());
            let prefix = session
                .begin_checked_prefix(admission.clone(), item.clone())
                .unwrap();
            let reservation = session.admit_checked_item(prefix.clone(), item).unwrap();
            // The external public lease does not own the detached private scope.
            drop(lease);
            drop(execution);
            session.reap_admission_leases();
            assert!(!session.scope_tree().is_live(public));
            assert!(session.scope_tree().is_live(scope));
            assert_eq!(session.public_visibility_snapshot_in(scope), Some(original));
            let commit = session.adopt_checked_declaration(reservation).unwrap();
            assert_eq!(commit.generation, Generation(1));
            let visible = session.public_visibility_snapshot_in(scope).unwrap();
            assert_eq!(visible.declaration_tip, Generation(1));
            assert_eq!(visible.epoch, 1);
            assert!(session
                .lib()
                .log
                .certified_authored_at(Generation(1))
                .is_some());
            assert_eq!(prefix.snapshot().compiler_prefix().next_item(), 1);
            assert_eq!(session.lib().scope_tip(ScopeId::ROOT), Generation(0));
            assert_eq!(
                session
                    .public_visibility_snapshot_in(ScopeId::ROOT)
                    .unwrap()
                    .epoch,
                0
            );
            drop(checked);
            drop(prefix);
            drop(admission);
            session.reap_admission_leases();
            assert!(!session.scope_tree().is_live(scope));
        }
    }

    #[test]
    fn admitted_cell_certifies_original_local_declaration_before_its_bind_and_expression() {
        use crate::session::{
            resident_cell_check_template, resident_workbench_templates, ModuleEnv,
            PersistentSession, SessionLib,
        };
        use tidepool_codegen::scope::ScopeId;
        use tidepool_repr::{SessionId, SessionModule};
        use tidepool_testing::effect_surface::TestEffectSurface;
        tidepool_testing::eval_harness::require_extract();
        let _daemon = TestEnvGuard::unset("TIDEPOOL_EXTRACT_DAEMON_SOCKET");
        let root = tempfile::tempdir().unwrap();
        let effects = TestEffectSurface::minimal(&[]).unwrap();
        let mut env = ModuleEnv::standalone_default();
        env.pragmas.push_str("\n{-# LANGUAGE TypeFamilies #-}");
        let mut lib = SessionLib::open(SessionId(995), root.path(), env)
            .unwrap()
            .with_validation_include(effects.include_paths().to_vec());
        lib.attach_recovery_graph_v2(&root.path().join("declarations.json"))
            .unwrap();
        let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
        let public = session.mint_scope(ScopeId::ROOT).unwrap();
        let prior_declarations = include_str!("fixtures/checked-prior-declaration.hs")
            .trim()
            .split("\n\n")
            .collect::<Vec<_>>();
        let inherited_generation = session
            .define_scoped_in(public, &prior_declarations)
            .unwrap();
        let inherited_surface = session.exact_exports_in(public, &["PriorNominal"]).unwrap();
        let inherited_exports = inherited_surface.declarations().unwrap().to_vec();
        let execution = Arc::new(session.begin_private_execution(public).unwrap());
        let view = execution.view();
        let imports = view.turn_imports(&crate::session::SourceImports::new());
        let expected_declaration = session.next_lib_module().unwrap();
        let declaration_header = format!("module {} where", expected_declaration.module_name());
        let declaration_preamble = effects
            .preamble()
            .replace("module Expr where", &declaration_header);
        assert!(declaration_preamble.contains(&declaration_header));
        let template = resident_cell_check_template(&declaration_preamble, effects.row(), &imports);
        let templates = resident_workbench_templates(effects.preamble(), effects.row(), &imports);
        let source = include_str!("fixtures/checked-local-declaration.hs").replace(
            "{{DECLARATION_MODULE}}",
            &expected_declaration.module_name(),
        );
        let source = source.as_str();
        let specification = CheckedCellSpecification {
            admission_digest: [0; 32],
            cell_source: source.into(),
            template_source: template.clone(),
            turn_templates: templates
                .iter()
                .map(|template| (template.kind.wire_name().into(), template.source.clone()))
                .collect(),
            injected_modules: view.injected_module_names(),
            reserved_declaration_modules: vec![],
        };
        let admitted_include = view.include_paths(effects.include_paths());
        let admission = session
            .admit_cell_for_execution(
                execution.clone(),
                1,
                Arc::new(specification.clone()),
                specification.specification_digest(),
                [1; 32],
                admitted_include,
            )
            .unwrap();
        assert_eq!(
            admission.reserved_generations(),
            &[expected_declaration.gen()]
        );
        let view = admission.view();
        let include = view.include_paths(effects.include_paths());
        let include = include.iter().map(PathBuf::as_path).collect::<Vec<_>>();
        let injected = view.injected_module_names();
        let checked = check_cell_admitted(
            CellCheckRequest {
                exact_context: view.exact_compile_context(),
                session_id: Some(view.session()),
                cell_text: source,
                template: &template,
                include: &include,
                session_root: view.session_root(),
                inject_modules: &injected,
                compile_generation: admission.initial_value_generation().0,
                compile_view_evidence: "",
            },
            admission.clone(),
            &templates,
        )
        .unwrap();
        assert_eq!(checked.items.len(), 4);
        let declaration = checked.checked_item(0).unwrap();
        let certificate = declaration
            .planned_declaration()
            .expect("same-offer original declaration certificate");
        let module = SessionModule::lib(admission.reserved_generations()[0]).module_name();
        assert_eq!(certificate.product().owner().module, module);
        assert!(declaration
            .planned_declaration_source()
            .unwrap()
            .contains("module "));
        assert!(certificate
            .lexical_exports()
            .iter()
            .any(|export| export.head.occurrence == "LocalBox" && export.head.module == module));
        assert!(certificate
            .instances()
            .classes
            .iter()
            .any(|instance| instance.class.occurrence == "LocalClass"
                && instance.class.module == module));
        assert!(!certificate.instances().families.is_empty());
        let original_product = certificate.product().clone();
        let replacement = checked.checked_item(1).unwrap();
        let binding = checked.checked_item(2).unwrap();
        assert!(
            binding.signatures()[0]
                .names()
                .iter()
                .any(|name| name.module() == module && name.occurrence() == "LocalBox"),
            "local nominal signature: {:?}",
            binding.signatures()
        );
        let expression = checked.checked_item(3).unwrap();
        assert_eq!(
            expression.expression_lift().unwrap(),
            Some(tidepool_toolchain::checked_cell::CheckedExpressionLift::Pure)
        );
        let prepared_declaration = admission.prepared_declaration(&declaration).unwrap();
        let receipt = prepared_declaration.projection.receipt().clone();
        let prefix = declaration.initial_prefix().unwrap();
        assert!(prefix
            .append_declaration_with_projection(binding.clone(), receipt.clone())
            .is_err());
        let prefix = prefix
            .append_declaration_with_projection(declaration.clone(), receipt.clone())
            .unwrap();
        assert_eq!(prefix.next_item(), 1);
        assert_eq!(prefix.completed_declaration(0), Some(&declaration));
        assert!(prefix
            .append_declaration_with_projection(declaration.clone(), receipt)
            .is_err());
        let protected = session
            .begin_checked_prefix(admission, declaration.clone())
            .unwrap();
        let reservation = session
            .admit_checked_item(protected.clone(), declaration)
            .unwrap();
        #[derive(Clone)]
        struct QuietOutput;
        impl crate::session::OutputSink for QuietOutput {
            fn drain(&self) -> Vec<String> {
                Vec::new()
            }
            fn snapshot(&self) -> Vec<String> {
                Vec::new()
            }
        }
        let mut resident = crate::session::ResidentSession::from_persistent_for_test(
            frunk::HNil,
            QuietOutput,
            session,
        );
        resident
            .set_run_context(crate::session::SessionRunContext {
                lexical_scope: execution.private_scope(),
                ..Default::default()
            })
            .unwrap();
        resident
            .adopt_checked_declaration(reservation.clone())
            .unwrap();
        let selected = resident
            .exact_exports_in(execution.private_scope(), &["PriorNominal", "LocalBox"])
            .unwrap();
        assert!(selected
            .declarations()
            .unwrap()
            .contains(&inherited_exports[0]));
        let view = resident.compile_view_in(execution.private_scope()).unwrap();
        let facade = selected.materialize(&view).unwrap();
        assert!(facade.source_artifact().is_none());
        let projection = facade.projection().unwrap();
        assert!(projection
            .receipt()
            .exports()
            .contains(&inherited_exports[0]));
        assert!(projection
            .context()
            .lexical_graph()
            .iter()
            .any(|node| node.owner.module == projection.module_name()));
        assert!(!projection.context().lexical_graph().iter().any(
            |node| node.owner.module == SessionModule::lib(inherited_generation).module_name()
        ));
        assert_eq!(protected.snapshot().compiler_prefix().next_item(), 1);
        let adopted = protected.snapshot();
        let imports = adopted
            .view()
            .turn_imports(&crate::session::SourceImports::new());
        assert!(
            imports
                .lines()
                .any(|line| line == prepared_declaration.projection.module_name()),
            "new source imports the compiler-issued cumulative projection"
        );
        assert!(
            !imports.lines().any(|line| line == module),
            "the local native original is not the future lexical import"
        );
        assert_eq!(
            adopted.view().library().unwrap().module_name(),
            module,
            "the reserved native declaration identity remains unchanged"
        );
        assert!(adopted
            .view()
            .exact_declaration_context()
            .unwrap()
            .lexical_graph()
            .iter()
            .any(|node| node.owner.module == prepared_declaration.projection.module_name()));
        let source_inputs = crate::session::RuntimeCompileInputs::new(
            None,
            vec![
                prepared_declaration.projection.clone(),
                prepared_declaration.projection.clone(),
            ],
        )
        .unwrap();
        assert_eq!(
            source_inputs.projections().len(),
            1,
            "repeated transfer retains one exact issued capsule"
        );
        let projected = protected
            .admission()
            .view()
            .clone()
            .with_compile_inputs(&source_inputs)
            .unwrap();
        assert!(projected
            .exact_declaration_context()
            .unwrap()
            .lexical_graph()
            .iter()
            .any(|node| node.owner.module == prepared_declaration.projection.module_name()));
        let selected_inputs =
            crate::session::RuntimeCompileInputs::new(None, vec![projection.clone()]).unwrap();
        let shared = projected
            .clone()
            .with_compile_inputs(&selected_inputs)
            .unwrap();
        let repeated = shared
            .clone()
            .with_compile_inputs(&selected_inputs)
            .unwrap();
        assert_eq!(
            shared.exact_declaration_context(),
            repeated.exact_declaration_context()
        );
        assert_eq!(
            projected.exact_declaration_context(),
            projected
                .clone()
                .with_compile_inputs(&source_inputs)
                .unwrap()
                .exact_declaration_context()
        );
        let graph = shared.exact_declaration_context().unwrap().lexical_graph();
        assert_eq!(
            graph
                .iter()
                .map(|node| &node.owner)
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            graph.len()
        );
        assert!(resident.adopt_checked_declaration(reservation).is_err());
        let original_context = protected
            .snapshot()
            .view()
            .exact_declaration_context()
            .cloned();
        let value_items = [replacement, binding, expression];
        let expected_value_outputs = value_items.len() as u64;
        for item in value_items {
            let reservation = resident
                .admit_checked_item(protected.clone(), item.clone())
                .unwrap();
            let snapshot = reservation.snapshot();
            let current = snapshot.view();
            let include = current.include_paths(effects.include_paths());
            let include = include.iter().map(PathBuf::as_path).collect::<Vec<_>>();
            let injected = snapshot.compiler_prefix().injected_modules();
            let TurnResult::Bind {
                bound, compiled, ..
            } = run_checked_item(
                TurnRequest {
                    exact_context: current.exact_compile_context(),
                    session_id: Some(current.session()),
                    turn_text: item.source(),
                    templates: &templates,
                    include: &include,
                    session_root: current.session_root(),
                    inject_modules: &injected,
                    gen: reservation.generation().0,
                    verdict: Some(checked.items[item.index()].verdict.clone()),
                    target: None,
                    retained_imports: &[],
                },
                reservation.clone(),
            )
            .unwrap()
            else {
                panic!("ordered checked item must compile its binding recipe")
            };
            let proof = compiled
                .certification
                .as_ref()
                .unwrap()
                .checked_execution()
                .unwrap();
            proof
                .validate_runtime_admission(reservation.digest(), protected.admission().digest())
                .unwrap();
            assert!(proof
                .validate_runtime_admission([0; 32], protected.admission().digest())
                .is_err());
            proof
                .validate_settled_native_bindings(snapshot.settled_native_bindings())
                .unwrap();
            let mut edited_rows = snapshot
                .settled_native_bindings()
                .map(|(name, identity, generation, id)| {
                    (name.to_owned(), identity.clone(), generation, id)
                })
                .collect::<Vec<_>>();
            if let Some(row) = edited_rows.first_mut() {
                row.3 ^= 1;
                assert!(proof
                    .validate_settled_native_bindings(edited_rows.iter().map(
                        |(name, identity, generation, id)| (
                            name.as_str(),
                            identity,
                            *generation,
                            *id
                        )
                    ))
                    .is_err());
            }
            if item.kind() == tidepool_toolchain::checked_cell::CheckedItemKind::Bind {
                resident
                    .run_bind_with_sites(
                        &bound[0].name,
                        compiled.code(),
                        &bound[0],
                        reservation.generation(),
                    )
                    .unwrap();
            } else {
                resident
                    .run_observation_with_sites(
                        compiled.code(),
                        &bound[0],
                        reservation.generation(),
                        false,
                    )
                    .unwrap();
            }
            assert_eq!(
                protected.snapshot().compiler_prefix().next_item(),
                item.index() + 1
            );
            assert_eq!(
                protected.snapshot().view().exact_declaration_context(),
                original_context.as_ref(),
                "private value overlays must preserve the exact original declaration context"
            );
        }
        let private_winners = resident
            .public_visibility_snapshot_in(execution.private_scope())
            .unwrap()
            .bindings
            .into_iter()
            .filter(|(name, _)| name == "historical" || name == "local")
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(private_winners.len(), 2);
        let intent = resident.freeze_private_execution(&execution).unwrap();
        let crate::session::ExecutionPublication::Declarations(base) = resident
            .restage_ephemeral_execution_publication(intent)
            .unwrap()
        else {
            panic!("original declaration and value winners require paired publication")
        };
        let crate::session::CertifiedDeclarationPublication::Accepted(accepted) =
            base.certify().unwrap()
        else {
            panic!("the original declaration and certified private Value overlay must join")
        };
        assert_eq!(
            resident
                .publish_staged_public_manifest(
                    accepted.stage().unwrap(),
                    &crate::session::PublicationDecision::new()
                )
                .unwrap(),
            crate::session::PublicManifestCommit::Ephemeral
        );
        let public_bindings = resident.public_visibility_snapshot_in(public).unwrap();
        assert_eq!(
            public_bindings
                .bindings
                .into_iter()
                .filter(|(name, _)| name == "historical" || name == "local")
                .collect::<std::collections::BTreeMap<_, _>>(),
            private_winners,
            "publication changed the actual native Value winner IDs"
        );
        let public_context = resident.compile_view_in(public).unwrap();
        assert_eq!(
            public_context
                .exact_declaration_context()
                .unwrap()
                .recovery_products()
                .iter()
                .find(|product| product.owner().module == module),
            Some(&original_product),
            "publication changed the certified original product or interface bytes"
        );
        let input_work = checked.checked_item(0).unwrap().input_work();
        assert_eq!(input_work.initial_files_written, 0);
        assert_eq!(input_work.initial_bytes_written_and_hashed, 0);
        assert_eq!(input_work.output_files_hashed, expected_value_outputs);
        assert!(input_work.output_bytes_hashed > 0);
        eprintln!("checked-original input_work={input_work:?}");
    }

    #[test]
    fn paired_join_compiles_source_hidden_checked_bind_in_fresh_worker() {
        use crate::session::{
            resident_cell_check_template, resident_workbench_templates, run_inspections,
            CertifiedDeclarationPublication, InspectionQuery, InspectionRequest, InspectionResult,
            ModuleEnv, PersistentSession, PublicManifestCommit, PublicationDecision,
            RecoveryPublicOwner, SessionLib, SourceImports,
        };
        use tidepool_codegen::scope::ScopeId;
        use tidepool_repr::{SessionId, SessionModule};
        use tidepool_testing::effect_surface::TestEffectSurface;

        tidepool_testing::eval_harness::require_extract();
        let _daemon = TestEnvGuard::unset("TIDEPOOL_EXTRACT_DAEMON_SOCKET");
        let root = tempfile::tempdir().unwrap();
        let effects = TestEffectSurface::minimal(&[]).unwrap();
        let mut lib =
            SessionLib::open(SessionId(994), root.path(), ModuleEnv::standalone_default())
                .unwrap()
                .with_validation_include(effects.include_paths().to_vec());
        lib.attach_recovery_graph_v2(root.path().join("declarations.json"))
            .unwrap();
        let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
        let public = session.mint_scope(ScopeId::ROOT).unwrap();
        let private = session.mint_detached_scope(public).unwrap();
        let owner = RecoveryPublicOwner::new(
            &tidepool_repr::ActorPath::parse("root/source-hidden").unwrap(),
            1,
        )
        .unwrap();
        session
            .bind_durable_public_scope(owner.clone(), public)
            .unwrap();
        let admitted = session.public_visibility_snapshot_in(public).unwrap();
        let receipt = session
            .lib()
            .declaration_receipt(&[include_str!("fixtures/exact-join-original.hs")])
            .unwrap()
            .unwrap();
        let (candidate, values) = session
            .render_declaration_candidate_in(private, &receipt, &SourceImports::new())
            .unwrap();
        let staged =
            crate::session::validate_declaration_candidate(candidate, session.lib().include_dir())
                .unwrap()
                .with_visible_values(values);
        session.adopt_staged_declaration_in(staged).unwrap();
        let original_generation = session.lib().scope_tip(private);
        let original = session
            .lib()
            .log
            .certified_authored_arc_at(original_generation)
            .unwrap();
        session.retract_in(private, "HiddenResult").unwrap();
        let CertifiedDeclarationPublication::Accepted(accepted) = session
            .snapshot_declaration_publication(owner, &admitted, private, vec![], vec![])
            .unwrap()
            .certify()
            .unwrap()
        else {
            panic!("first authored declaration must be accepted");
        };
        assert_eq!(
            session
                .publish_staged_public_manifest(
                    accepted.stage().unwrap(),
                    &PublicationDecision::new(),
                )
                .unwrap(),
            PublicManifestCommit::Durable
        );
        session.retire_scope(private);

        let original_path = root
            .path()
            .join(SessionModule::lib(original_generation).relative_hs_path());
        assert!(original_path.exists());
        std::fs::remove_file(&original_path).unwrap();
        let interface_path = original_path.with_extension("hi");
        if interface_path.exists() {
            std::fs::remove_file(&interface_path).unwrap();
        }
        let view = session.compile_view_in(public).unwrap();
        let context = view.exact_declaration_context().unwrap();
        assert_ne!(
            view.library(),
            Some(SessionModule::lib(original_generation))
        );
        assert!(context
            .lexical_graph()
            .iter()
            .any(|node| node.owner.module == view.library().unwrap().module_name()));
        assert!(context
            .lexical_graph()
            .iter()
            .any(|node| node.owner.module == "Tidepool.Data.Text"));
        assert!(context
            .lexical_graph()
            .iter()
            .all(|node| node.owner.module != original.product().owner().module));

        let imports = view.turn_imports(&SourceImports::new());
        let include = view.include_paths(effects.include_paths());
        let include = include.iter().map(PathBuf::as_path).collect::<Vec<_>>();
        let injected = view.injected_module_names();
        let hidden_imports = format!(
            "{imports}\nqualified {} as HiddenOriginal",
            original.product().owner().module
        );
        let hidden_queries = [InspectionQuery::TypeOf(
            "HiddenOriginal.answer (41 :: Int)".into(),
        )];
        let hidden = run_inspections(InspectionRequest {
            exact_context: Some(Arc::new(
                tidepool_toolchain::declaration_join::ExactCompileContext::new(context.clone()),
            )),
            preamble: effects.preamble(),
            imports: &hidden_imports,
            include: &include,
            session_root: view.session_root(),
            inject_modules: &injected,
            queries: &hidden_queries,
            effects: Some(effects.row()),
        });
        match hidden {
            Err(_) => {}
            Ok(results) => assert!(
                results
                    .iter()
                    .all(|result| matches!(result, InspectionResult::Rejected { .. })),
                "retained original implementation must remain unavailable to textual imports"
            ),
        }
        let mixed_queries = [
            InspectionQuery::Info("answer".into()),
            InspectionQuery::ScopeBrowse,
            InspectionQuery::TypeOf("missingOriginalName".into()),
            InspectionQuery::TypeOf("answer (41 :: Int)".into()),
            InspectionQuery::TypeOf("(undefined :: HiddenResult)".into()),
        ];
        let mixed = run_inspections(InspectionRequest {
            exact_context: Some(Arc::new(
                tidepool_toolchain::declaration_join::ExactCompileContext::new(context.clone()),
            )),
            preamble: effects.preamble(),
            imports: &imports,
            include: &include,
            session_root: view.session_root(),
            inject_modules: &injected,
            queries: &mixed_queries,
            effects: Some(effects.row()),
        })
        .unwrap();
        assert!(matches!(&mixed[0], InspectionResult::Info { entries, .. } if !entries.is_empty()));
        assert!(matches!(&mixed[1], InspectionResult::Browse { .. }));
        assert!(matches!(&mixed[2], InspectionResult::Rejected { .. }));
        assert!(matches!(&mixed[3], InspectionResult::Type { display, .. } if display == "Int"));
        assert!(
            matches!(&mixed[4], InspectionResult::Rejected { .. }),
            "hidden nominal head entered lexical scope"
        );
        let batch_queries = [
            InspectionQuery::TypeOf("answer".into()),
            InspectionQuery::TypeOf("answer (42 :: Int)".into()),
        ];
        let batch = run_inspections(InspectionRequest {
            exact_context: Some(Arc::new(
                tidepool_toolchain::declaration_join::ExactCompileContext::new(context.clone()),
            )),
            preamble: effects.preamble(),
            imports: &imports,
            include: &include,
            session_root: view.session_root(),
            inject_modules: &injected,
            queries: &batch_queries,
            effects: Some(effects.row()),
        })
        .unwrap();
        assert!(batch
            .iter()
            .all(|result| matches!(result, InspectionResult::Type { .. })));
        let template = resident_cell_check_template(effects.preamble(), effects.row(), &imports);
        let templates = resident_workbench_templates(effects.preamble(), effects.row(), &imports);
        let admission_specification = CheckedCellSpecification {
            admission_digest: [0; 32],
            cell_source: "let result = makeResult (41 :: Int)".into(),
            template_source: template.clone(),
            turn_templates: templates
                .iter()
                .map(|template| (template.kind.wire_name().into(), template.source.clone()))
                .collect(),
            injected_modules: injected.clone(),
            reserved_declaration_modules: Vec::new(),
        };
        let binding_execution = Arc::new(session.begin_private_execution(public).unwrap());
        let binding_scope = binding_execution.private_scope();
        let admission = session
            .admit_cell_for_execution(
                binding_execution.clone(),
                0,
                Arc::new(admission_specification.clone()),
                admission_specification.specification_digest(),
                [0; 32],
                include.iter().map(|path| path.to_path_buf()).collect(),
            )
            .unwrap();
        let checked = check_cell_admitted(
            CellCheckRequest {
                exact_context: Some(Arc::new(
                    tidepool_toolchain::declaration_join::ExactCompileContext::new(context.clone()),
                )),
                session_id: Some(view.session()),
                cell_text: "let result = makeResult (41 :: Int)",
                template: &template,
                include: &include,
                session_root: view.session_root(),
                inject_modules: &injected,
                compile_generation: view.next_value_generation().0,
                compile_view_evidence: "",
            },
            admission.clone(),
            &templates,
        )
        .unwrap();
        assert_eq!(checked.items.len(), 1);
        let item = checked.checked_item(0).unwrap();
        assert_eq!(item.admission_digest(), admission.digest());
        assert_eq!(item.source(), checked.items[0].source);
        assert_eq!(item.signatures().len(), 1);
        assert!(item
            .validate_observations(
                "let result = makeResult (0 :: Int)",
                tidepool_toolchain::checked_cell::CheckedItemKind::Bind,
                item.binders(),
                &[],
                &[]
            )
            .is_err());
        assert!(item.signatures()[0]
            .names()
            .iter()
            .any(|name| name.module() == original.product().owner().module
                && name.occurrence() == "HiddenResult"));
        let prefix = session
            .begin_checked_prefix(admission.clone(), item.clone())
            .unwrap();
        let item_admission = session.admit_checked_item(prefix, item.clone()).unwrap();
        let binding_prefix = item_admission.prefix().clone();
        let TurnResult::Bind {
            bound, compiled, ..
        } = run_checked_item(
            TurnRequest {
                exact_context: Some(Arc::new(
                    tidepool_toolchain::declaration_join::ExactCompileContext::new(context.clone()),
                )),
                session_id: Some(view.session()),
                turn_text: &checked.items[0].source,
                templates: &templates,
                include: &include,
                session_root: view.session_root(),
                inject_modules: &injected,
                gen: view.next_value_generation().0,
                verdict: Some(checked.items[0].verdict.clone()),
                target: None,
                retained_imports: &[],
            },
            item_admission,
        )
        .unwrap()
        else {
            panic!("source-hidden Join consumer must be a bind");
        };
        assert_eq!(bound[0].name, "result");
        assert!(
            bound[0].type_display.ends_with("HiddenResult"),
            "{}",
            bound[0].type_display
        );
        let certification = compiled.certification.as_ref().unwrap();
        assert!(certification
            .checked_execution()
            .unwrap()
            .shares_target(&compiled.prepared));
        certification
            .validate_checked_bind(&compiled.prepared, view.next_value_generation().0, &bound)
            .unwrap();
        let mut edited = bound.clone();
        edited[0].name = "edited".into();
        assert!(certification
            .validate_checked_bind(&compiled.prepared, view.next_value_generation().0, &edited)
            .is_err());
        assert!(
            certification
                .groups
                .iter()
                .any(|group| group.owner() == original.product().owner()),
            "consumer must retain the original owned group identity"
        );
        let mut edited_check = checked.clone();
        edited_check.pins[0].ty = "Int".into();
        assert!(edited_check.checked_item(0).is_err());
        let mut edited_check = checked.clone();
        edited_check.items[0].source = "let result = makeResult (0 :: Int)".into();
        assert!(edited_check.checked_item(0).is_err());
        let expression_view = session.compile_view_in(public).unwrap();
        let expression_source = "makeResult (42 :: Int)";
        let expression_specification = CheckedCellSpecification {
            admission_digest: [0; 32],
            cell_source: expression_source.into(),
            template_source: template.clone(),
            turn_templates: templates
                .iter()
                .map(|template| (template.kind.wire_name().into(), template.source.clone()))
                .collect(),
            injected_modules: injected.clone(),
            reserved_declaration_modules: Vec::new(),
        };
        let expression_execution = Arc::new(session.begin_private_execution(public).unwrap());
        let expression_scope = expression_execution.private_scope();
        let expression_admission = session
            .admit_cell_for_execution(
                expression_execution.clone(),
                0,
                Arc::new(expression_specification.clone()),
                expression_specification.specification_digest(),
                [0; 32],
                include.iter().map(|path| path.to_path_buf()).collect(),
            )
            .unwrap();
        let expression_check = check_cell_admitted(
            CellCheckRequest {
                exact_context: Some(Arc::new(
                    tidepool_toolchain::declaration_join::ExactCompileContext::new(context.clone()),
                )),
                session_id: Some(view.session()),
                cell_text: expression_source,
                template: &template,
                include: &include,
                session_root: view.session_root(),
                inject_modules: &injected,
                compile_generation: expression_view.next_value_generation().0,
                compile_view_evidence: "",
            },
            expression_admission.clone(),
            &templates,
        )
        .unwrap();
        let expression_item = expression_check.checked_item(0).unwrap();
        assert!(expression_item.signatures()[0]
            .names()
            .iter()
            .any(|name| name.module() == original.product().owner().module
                && name.occurrence() == "HiddenResult"));
        let expression_prefix = session
            .begin_checked_prefix(expression_admission, expression_item.clone())
            .unwrap();
        let expression_reservation = session
            .admit_checked_item(expression_prefix, expression_item)
            .unwrap();
        let expression_request = |templates: &[TurnTemplate]| {
            run_checked_item(
                TurnRequest {
                    exact_context: Some(Arc::new(
                        tidepool_toolchain::declaration_join::ExactCompileContext::new(
                            context.clone(),
                        ),
                    )),
                    session_id: Some(view.session()),
                    turn_text: expression_source,
                    templates,
                    include: &include,
                    session_root: view.session_root(),
                    inject_modules: &injected,
                    gen: expression_reservation.generation().0,
                    verdict: Some(expression_check.items[0].verdict.clone()),
                    target: None,
                    retained_imports: &[],
                },
                expression_reservation.clone(),
            )
        };
        let mut edited_templates = templates.clone();
        edited_templates[0].source.push_str("\n-- edited wrapper\n");
        assert!(expression_request(&edited_templates).is_err());
        let TurnResult::Bind {
            variant,
            bound: expression_bound,
            compiled: expression_compiled,
            ..
        } = expression_request(&templates).unwrap()
        else {
            panic!("checked expression must compile its certified capture recipe");
        };
        assert_eq!(
            variant, 0,
            "hidden nominal expression capture must use its canonical recipe"
        );
        assert!(expression_compiled
            .certification
            .as_ref()
            .unwrap()
            .checked_execution()
            .unwrap()
            .matches_target(&expression_compiled.prepared));
        assert!(!original_path.exists());
        assert!(view.is_current_for(&session.compile_view_in(public).unwrap()));
        #[derive(Clone)]
        struct QuietOutput;
        impl crate::session::OutputSink for QuietOutput {
            fn drain(&self) -> Vec<String> {
                Vec::new()
            }
            fn snapshot(&self) -> Vec<String> {
                Vec::new()
            }
        }
        let mut resident = crate::session::ResidentSession::from_persistent_for_test(
            frunk::HNil,
            QuietOutput,
            session,
        );
        resident
            .set_run_context(crate::session::SessionRunContext {
                lexical_scope: binding_scope,
                ..Default::default()
            })
            .unwrap();
        assert!(resident
            .run_bind_with_sites(
                "edited",
                compiled.code(),
                &edited[0],
                view.next_value_generation()
            )
            .is_err());
        assert!(
            !resident.prepared_machine_ready(),
            "edited binder metadata reached native install"
        );
        assert_eq!(binding_prefix.snapshot().compiler_prefix().next_item(), 0);
        resident
            .set_run_context(crate::session::SessionRunContext {
                lexical_scope: expression_scope,
                ..Default::default()
            })
            .unwrap();
        let outcome = resident
            .run_observation_with_sites(
                expression_compiled.code(),
                &expression_bound[0],
                expression_reservation.generation(),
                false,
            )
            .unwrap();
        match outcome {
            crate::session::ResidentOutcome::Completed { .. }
            | crate::session::ResidentOutcome::BindingsCommitted { .. } => {}
            other => panic!("hidden nominal expression did not complete: {other:?}"),
        }
        assert_eq!(
            expression_reservation
                .prefix()
                .snapshot()
                .compiler_prefix()
                .next_item(),
            1
        );
        resident
            .set_run_context(crate::session::SessionRunContext {
                lexical_scope: binding_scope,
                ..Default::default()
            })
            .unwrap();
        let outcome = resident
            .run_bind_with_sites(
                "checked",
                compiled.code(),
                &bound[0],
                view.next_value_generation(),
            )
            .unwrap();
        assert!(matches!(
            outcome,
            crate::session::ResidentOutcome::Completed { .. }
                | crate::session::ResidentOutcome::BindingsCommitted { .. }
        ));
        assert_eq!(binding_prefix.snapshot().compiler_prefix().next_item(), 1);
        assert!(resident
            .run_bind_with_sites(
                "replayed",
                compiled.code(),
                &bound[0],
                view.next_value_generation()
            )
            .is_err());
    }

    use super::*;

    #[test]
    fn generic_tool_policy_certifies_canonical_constraint_tuple_selector() {
        use tidepool_repr::{Generation, SessionModule};
        use tidepool_testing::effect_surface::TestEffectSurface;
        use tidepool_toolchain::artifacts::{compile_invocation, CompileInvocation};

        tidepool_testing::eval_harness::require_extract();
        let surface = TestEffectSurface::minimal(&[tidepool_mcp::agent_tools_decl()])
            .expect("owned AgentTools effect surface");
        let source_root = tempfile::tempdir().unwrap();
        let path = source_root
            .path()
            .join(SessionModule::lib(Generation(1)).relative_hs_path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, include_str!("fixtures/constraint-tuple-G1.hs")).unwrap();
        let mut includes = vec![source_root.path().to_path_buf()];
        includes.extend_from_slice(surface.include_paths());
        let artifacts = compile_invocation(&CompileInvocation {
            source: "module TidepoolConstraintTupleProbe where\nimport qualified Tidepool.Session.Lib.G1 as Original\nresult = Original.policy\n",
            targets: &["result"],
            include: &includes,
            fallback_module_name: "TidepoolConstraintTupleProbe",
        }, |_, _, _| {}).expect("generic policy must retain a complete package witness");
        let imports = artifacts
            .certified_groups
            .iter()
            .flat_map(|group| group.imports())
            .chain(
                artifacts
                    .targets
                    .values()
                    .flat_map(|target| target.pending_imports.iter()),
            );
        assert!(imports.into_iter().any(|owner| matches!(owner,
            PendingImportOwner::Package { unit, module, binder, interface_digest }
                if binder.occurrence == "$p1CTuple2"
                    && unit == &binder.unit
                    && module == &binder.module
                    && *interface_digest != [0; 32]
        )), "generic policy must certify the original constraint-tuple selector as an exact package import");
    }

    /// Force tests that replace `TIDEPOOL_EXTRACT` to exercise that process
    /// boundary even when the surrounding test runner owns a compile daemon.
    /// Each native libtest case has its own process, but it still inherits the
    /// runner's daemon socket.
    pub(super) struct TestEnvGuard {
        key: &'static str,
        old: Option<std::ffi::OsString>,
    }

    impl TestEnvGuard {
        pub(super) fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let old = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, old }
        }

        pub(super) fn unset(key: &'static str) -> Self {
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

    /// The compiled-cell inference fixtures are acceptance gates. Require the
    /// matched frontend and worker explicitly so an unconfigured run cannot
    /// report their early return as a passing test.
    fn required_cell_test_paths(
        frontend: Option<std::ffi::OsString>,
        worker: Option<std::ffi::OsString>,
    ) -> (std::ffi::OsString, std::ffi::OsString) {
        let frontend = frontend
            .expect("TIDEPOOL_CELL_TEST_EXTRACT is required for compiled-cell fixture tests");
        let worker =
            worker.expect("TIDEPOOL_EXTRACT_WORKER is required for compiled-cell fixture tests");
        use tidepool_toolchain::toolchain::{probe_extract_binary, ExtractBinaryRole};

        let frontend_role = probe_extract_binary(std::path::Path::new(&frontend));
        assert_eq!(
            frontend_role,
            ExtractBinaryRole::Frontend,
            "TIDEPOOL_CELL_TEST_EXTRACT must name the Tidepool frontend"
        );
        let worker_role = probe_extract_binary(std::path::Path::new(&worker));
        assert_eq!(
            worker_role,
            ExtractBinaryRole::Worker,
            "TIDEPOOL_EXTRACT_WORKER must name the matched Haskell compiler worker"
        );
        (frontend, worker)
    }

    fn required_cell_test_worker() -> std::ffi::OsString {
        required_cell_test_paths(
            std::env::var_os("TIDEPOOL_CELL_TEST_EXTRACT"),
            std::env::var_os("TIDEPOOL_EXTRACT_WORKER"),
        )
        .0
    }

    #[test]
    #[should_panic(expected = "TIDEPOOL_EXTRACT_WORKER is required")]
    fn compiled_cell_fixture_rejects_a_missing_worker() {
        let _ = required_cell_test_paths(Some("frontend".into()), None);
    }

    #[test]
    fn whole_cell_check_harvests_downstream_fixed_local_type() {
        let extract = required_cell_test_worker();
        let _daemon = TestEnvGuard::unset("TIDEPOOL_EXTRACT_DAEMON_SOCKET");
        let _extract = TestEnvGuard::set("TIDEPOOL_EXTRACT", extract);
        let root = tempfile::tempdir().unwrap();
        let prelude = tidepool_testing::eval_harness::prelude_path();
        let effects = tidepool_testing::eval_harness::effects_include();
        let include = [
            prelude.as_path(),
            effects[0].as_path(),
            effects[1].as_path(),
        ];
        let template = concat!(
            "{-# LANGUAGE NoImplicitPrelude #-}\n",
            "{{CELL_PRAGMAS}}\n",
            "module CellCheck where\n",
            "import Prelude\n",
            "{{CELL_IMPORTS}}\n",
            "__tidepoolCellExpression :: value -> IO ()\n",
            "__tidepoolCellExpression _ = pure ()\n",
            "__tidepoolInEffectRow :: IO value -> IO value\n",
            "__tidepoolInEffectRow = id\n",
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
            exact_context: None,
            session_id: None,
            cell_text: cell,
            template,
            include: &include,
            session_root: root.path(),
            inject_modules: &[],
            compile_generation: 0,
            compile_view_evidence: "",
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
    }

    #[test]
    fn whole_cell_check_harvests_same_cell_nominal_type() {
        let extract = required_cell_test_worker();
        let _daemon = TestEnvGuard::unset("TIDEPOOL_EXTRACT_DAEMON_SOCKET");
        let _extract = TestEnvGuard::set("TIDEPOOL_EXTRACT", extract);
        let root = tempfile::tempdir().unwrap();
        let prelude = tidepool_testing::eval_harness::prelude_path();
        let effects = tidepool_testing::eval_harness::effects_include();
        let include = [
            prelude.as_path(),
            effects[0].as_path(),
            effects[1].as_path(),
        ];
        let template = concat!(
            "{-# LANGUAGE NoImplicitPrelude #-}\n",
            "{{CELL_PRAGMAS}}\n",
            "module CellCheck where\n",
            "import Prelude\n",
            "{{CELL_IMPORTS}}\n",
            "__tidepoolCellExpression :: value -> IO ()\n",
            "__tidepoolCellExpression _ = pure ()\n",
            "__tidepoolInEffectRow :: IO value -> IO value\n",
            "__tidepoolInEffectRow = id\n",
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
            exact_context: None,
            session_id: None,
            cell_text: cell,
            template,
            include: &include,
            session_root: root.path(),
            inject_modules: &[],
            compile_generation: 0,
            compile_view_evidence: "",
        })
        .unwrap();
        let pins = checked
            .pins
            .iter()
            .filter(|pin| pin.key == "__tidepool_cell_pin_1_h")
            .collect::<Vec<_>>();
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
        let expression = checked
            .expression_plans
            .iter()
            .find(|plan| plan.key == "__tidepool_cell_expr_3")
            .expect("same-cell nominal expression observation");
        assert_eq!(expression.type_display, "Maybe G");
        assert!(expression
            .heads
            .iter()
            .any(|head| head.module == "CellCheck" && head.name == "G"));
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
            "{}\n{{-# LANGUAGE PolyKinds #-}}\nmodule CellCheck where\nimport Prelude\nimport Data.Text (Text)\n\
             {PREAMBLE_DEFAULT_DECL}",
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

    fn runtime_eff_cell_preamble() -> String {
        format!(
            "{}\nmodule CellCheck where\nimport Prelude\nimport Data.Text (Text)\n\
             import Control.Monad.Freer (Eff)\n\
             import Data.Proxy (Proxy(..))\n\
             {PREAMBLE_DEFAULT_DECL}",
            crate::session::EVAL_PRAGMAS,
        )
    }

    #[test]
    fn whole_cell_check_reports_missing_record_fields_without_rejecting_declaration() {
        let extract = required_cell_test_worker();
        let _daemon = TestEnvGuard::unset("TIDEPOOL_EXTRACT_DAEMON_SOCKET");
        let _extract = TestEnvGuard::set("TIDEPOOL_EXTRACT", extract);
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("Prior.hs"),
            "{-# OPTIONS_GHC -Wmissing-fields #-}\nmodule Prior where\n\
             data Old = Old { oldHead :: Int, oldTail :: Int }\n\
             {-# LINE 1 \"<cell>\" #-}\nold = Old { oldHead = 1 }\n",
        )
        .unwrap();
        let prelude = tidepool_testing::eval_harness::prelude_path();
        let effects = tidepool_testing::eval_harness::effects_include();
        let include = [
            root.path(),
            prelude.as_path(),
            effects[0].as_path(),
            effects[1].as_path(),
        ];
        let template = super::super::workbench::resident_cell_check_template(
            &runtime_eff_cell_preamble(),
            EFF_ROW,
            "Prior",
        );
        let declaration = "data MergeRequest = MergeRequest { mergeSourceHead :: Int, mergeSourceWorktree :: Int }\n";
        let partial = format!("{declaration}request = MergeRequest {{ mergeSourceHead = 1 }}\n");
        let checked = check_cell(CellCheckRequest {
            exact_context: None,
            session_id: None,
            cell_text: &partial,
            template: &template,
            include: &include,
            session_root: root.path(),
            inject_modules: &[],
            compile_generation: 0,
            compile_view_evidence: "",
        })
        .expect("partial record is a valid declaration");
        assert!(
            checked.warnings.iter().any(|warning| {
                warning.severity == crate::diag::DiagnosticSeverity::Warning
                    && warning
                        .span
                        .as_ref()
                        .is_some_and(|span| span.file == "<cell>" && span.start_line == 2)
                    && warning.message.contains("mergeSourceWorktree")
            }),
            "warnings: {:?}",
            checked.warnings
        );
        assert!(
            checked.warnings.iter().all(|warning| warning
                .span
                .as_ref()
                .is_some_and(|span| span.file == "<cell>")
                && !warning.message.contains("oldTail")),
            "dependency warnings leaked into the cell: {:?}",
            checked.warnings
        );

        let complete = format!(
            "{declaration}request = MergeRequest {{ mergeSourceHead = 1, mergeSourceWorktree = 2 }}\n"
        );
        let checked = check_cell(CellCheckRequest {
            exact_context: None,
            session_id: None,
            cell_text: &complete,
            template: &template,
            include: &include,
            session_root: root.path(),
            inject_modules: &[],
            compile_generation: 0,
            compile_view_evidence: "",
        })
        .expect("complete record declaration");
        assert!(
            checked.warnings.is_empty(),
            "warnings: {:?}",
            checked.warnings
        );
    }

    /// The checked plan classifies pure values and actions in the owned effect
    /// row, retaining quantified result types without defaulting their binders.
    #[test]
    fn checked_expression_plans_cover_pure_and_effectful_values() {
        let extract = required_cell_test_worker();
        let _daemon = TestEnvGuard::unset("TIDEPOOL_EXTRACT_DAEMON_SOCKET");
        let _extract = TestEnvGuard::set("TIDEPOOL_EXTRACT", extract);
        let root = tempfile::tempdir().unwrap();
        let prelude = tidepool_testing::eval_harness::prelude_path();
        let effects = tidepool_testing::eval_harness::effects_include();
        let include = [
            prelude.as_path(),
            effects[0].as_path(),
            effects[1].as_path(),
        ];
        let template = super::super::workbench::resident_cell_check_template(
            &runtime_eff_cell_preamble(),
            EFF_ROW,
            "",
        );
        for (cell, lift) in [
            ("1 :: Int\n", ExpressionLift::Pure),
            ("(pure (1 :: Int) :: IO Int)\n", ExpressionLift::Pure),
            ("pure (1 :: Int)\n", ExpressionLift::Effectful),
            (
                "pure (pure (1 :: Int) :: Eff '[] Int)\n",
                ExpressionLift::Effectful,
            ),
            ("pure Proxy\n", ExpressionLift::Effectful),
            ("pure (Proxy @3)\n", ExpressionLift::Effectful),
            ("pure const\n", ExpressionLift::Effectful),
        ] {
            let evidence = "compile-view-a";
            let checked = check_cell(CellCheckRequest {
                exact_context: None,
                session_id: None,
                cell_text: cell,
                template: &template,
                include: &include,
                session_root: root.path(),
                inject_modules: &[],
                compile_generation: 7,
                compile_view_evidence: evidence,
            })
            .unwrap_or_else(|failure| panic!("{cell:?}: {:?}", failure.error));
            let index = checked.items.len() - 1;
            let plan = checked
                .expression_plans
                .iter()
                .find(|plan| plan.key == format!("__tidepool_cell_expr_{index}"))
                .expect("checked expression observation");
            assert_eq!(plan.lift, lift);
            assert!(
                !plan.type_display.contains("ZonkAny"),
                "{}",
                plan.type_display
            );
            if cell == "pure Proxy\n" {
                assert!(
                    plan.type_display.contains("forall cell0")
                        && plan.type_display.contains("cell1 :: cell0"),
                    "{}",
                    plan.type_display
                );
            } else if cell == "pure const\n" {
                assert!(
                    plan.type_display.contains("forall cell0 cell1."),
                    "{}",
                    plan.type_display
                );
            }
        }
    }

    /// A polymorphic `pure` expression defaults its constructor to the exact
    /// workbench effect row during the one whole-cell typecheck.
    #[test]
    fn a_final_pure_cell_is_accepted_as_effectful() {
        let extract = required_cell_test_worker();
        let _daemon = TestEnvGuard::unset("TIDEPOOL_EXTRACT_DAEMON_SOCKET");
        let _extract = TestEnvGuard::set("TIDEPOOL_EXTRACT", extract);
        let root = tempfile::tempdir().unwrap();
        let prelude = tidepool_testing::eval_harness::prelude_path();
        let effects = tidepool_testing::eval_harness::effects_include();
        let include = [
            prelude.as_path(),
            effects[0].as_path(),
            effects[1].as_path(),
        ];
        let template = super::super::workbench::resident_cell_check_template(
            &eff_cell_preamble(),
            EFF_ROW,
            "",
        );
        let cell = format!("{EFF_DECLS}pure (1 :: Int)\n");

        // Named class defaulting selects the exact effect row in the first
        // whole-cell check; no diagnostic-triggered retry is involved.
        let checked = check_cell(CellCheckRequest {
            exact_context: None,
            session_id: None,
            cell_text: &cell,
            template: &template,
            include: &include,
            session_root: root.path(),
            inject_modules: &[],
            compile_generation: 0,
            compile_view_evidence: "",
        })
        .expect("a final `pure <expr>` cell must default in the compiler-owned row");
        let final_item = checked.items.last().expect("cell has at least one item");
        assert_eq!(final_item.verdict.kind, TurnKind::Expr);
        assert_eq!(final_item.source, "pure (1 :: Int)\n");
        let plan = checked
            .expression_plans
            .last()
            .expect("checked expression observation");
        // The helper type owns `Eff` identity, so this isolated fixture's
        // local stand-in is still the exact constructor selected by its own
        // cell template.
        assert_eq!(plan.lift, ExpressionLift::Effectful);
    }

    /// A genuinely pure final expression (no `Applicative`/`Monad` ambiguity
    /// at all — `1 + 1 :: Int` is not `Eff` anything) takes exactly the same
    /// path it always has: accepted on the first [`check_cell`] attempt, no
    /// retry involved.
    #[test]
    fn a_genuinely_pure_final_expression_still_takes_the_pure_path() {
        let extract = required_cell_test_worker();
        let _daemon = TestEnvGuard::unset("TIDEPOOL_EXTRACT_DAEMON_SOCKET");
        let _extract = TestEnvGuard::set("TIDEPOOL_EXTRACT", extract);
        let root = tempfile::tempdir().unwrap();
        let prelude = tidepool_testing::eval_harness::prelude_path();
        let effects = tidepool_testing::eval_harness::effects_include();
        let include = [
            prelude.as_path(),
            effects[0].as_path(),
            effects[1].as_path(),
        ];
        let template = super::super::workbench::resident_cell_check_template(
            &eff_cell_preamble(),
            EFF_ROW,
            "",
        );
        let cell = format!("{EFF_DECLS}1 + 1 :: Int\n");

        let checked = check_cell(CellCheckRequest {
            exact_context: None,
            session_id: None,
            cell_text: &cell,
            template: &template,
            include: &include,
            session_root: root.path(),
            inject_modules: &[],
            compile_generation: 0,
            compile_view_evidence: "",
        })
        .expect("a genuinely pure final expression must still be accepted outright");
        let final_item = checked.items.last().expect("cell has at least one item");
        let plan = checked
            .expression_plans
            .last()
            .expect("checked expression observation");
        assert_eq!(plan.lift, ExpressionLift::Pure);
        assert_eq!(final_item.verdict.kind, TurnKind::Expr);
        assert_eq!(final_item.source, "1 + 1 :: Int\n");

        // Repeating the same request makes the same compiler-owned decision.
        let repeated = check_cell(CellCheckRequest {
            exact_context: None,
            session_id: None,
            cell_text: &cell,
            template: &template,
            include: &include,
            session_root: root.path(),
            inject_modules: &[],
            compile_generation: 0,
            compile_view_evidence: "",
        })
        .expect("the repeated check must accept the same source");
        assert_eq!(repeated.items.last().unwrap().source, final_item.source);
    }

    /// The MCP producer uses the runtime-owned template marker.
    #[test]
    fn preamble_default_marker_matches_mcp_constant() {
        assert_eq!(PREAMBLE_DEFAULT_DECL, tidepool_mcp::PREAMBLE_DEFAULT_DECL);
        assert_eq!(PREAMBLE_IMPORT_MARKER, tidepool_mcp::PREAMBLE_IMPORT_MARKER);
        assert!(PREAMBLE_DEFAULT_DECL.starts_with(PREAMBLE_IMPORT_MARKER));
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
            exact_context: None,
            session_id: None,
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
            retained_imports: &[],
        };
        let err = run_turn(req).unwrap_err();
        assert!(
            matches!(err.error, CompileError::ExtractFailed(_)),
            "expected a clean ExtractFailed, got {err:?}"
        );
    }

    #[test]
    fn run_turn_retries_expression_template_after_source_rejection() {
        tidepool_testing::eval_harness::require_extract();
        let effects = tidepool_testing::effect_surface::TestEffectSurface::minimal(&[]).unwrap();
        let session_root = TempDir::new().unwrap();
        let templates = [
            TurnTemplate {
                kind: TemplateSelector::Expr,
                source: assemble_expression_module(
                    effects.preamble(),
                    "__result",
                    effects.row(),
                    "({{TURN}} :: Bool)",
                    ExpressionLift::Pure,
                ),
            },
            TurnTemplate {
                kind: TemplateSelector::Expr,
                source: assemble_opaque_expression_module(
                    effects.preamble(),
                    "__result",
                    effects.row(),
                    "{{TURN}}",
                    ExpressionLift::Pure,
                ),
            },
        ];
        let result = run_turn(TurnRequest {
            exact_context: None,
            session_id: None,
            turn_text: "((+ 1) :: Int -> Int)",
            templates: &templates,
            include: &effects.include_path_refs(),
            session_root: session_root.path(),
            inject_modules: &[],
            gen: 0,
            verdict: Some(TurnClassification {
                kind: TurnKind::Expr,
                binders: Vec::new(),
                items: Vec::new(),
            }),
            target: None,
            retained_imports: &[],
        })
        .unwrap();
        let TurnResult::Expr {
            variant,
            wrapped_source,
            ..
        } = result
        else {
            panic!("ordinary opaque alternative must remain an expression");
        };
        assert_eq!(variant, 1);
        assert_eq!(
            wrapped_source,
            templates[1]
                .source
                .replace("{{TURN}}", "((+ 1) :: Int -> Int)")
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
            exact_context: None,
            session_id: None,
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
            retained_imports: &[],
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
        assert_eq!(
            err.attempted_source.as_deref(),
            Some(templates[0].source.replace("{{TURN}}", "1 :: Int").as_str())
        );
    }

    #[test]
    fn failed_turn_preserves_the_last_attempted_template() {
        tidepool_testing::eval_harness::require_extract();
        let session_root = TempDir::new().unwrap();
        let templates = ["missingFirst", "missingLast"].map(|name| TurnTemplate {
            kind: TemplateSelector::Expr,
            source: format!("module Expr where\n__result = {name}\n"),
        });
        let failure = run_turn(TurnRequest {
            exact_context: None,
            session_id: None,
            turn_text: "()",
            templates: &templates,
            include: &[],
            session_root: session_root.path(),
            inject_modules: &[],
            gen: 0,
            verdict: Some(TurnClassification {
                kind: TurnKind::Expr,
                binders: vec![],
                items: vec![],
            }),
            target: None,
            retained_imports: &[],
        })
        .expect_err("both typed alternatives are rejected");
        assert_eq!(
            failure.attempted_source.as_deref(),
            Some(templates[1].source.as_str())
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
                    CborValue::Text("ForceData".into()),
                    CborValue::Text("Int".into()),
                    CborValue::Array(vec![
                        CborValue::Text("main".into()),
                        CborValue::Text("GHC.Types".into()),
                        CborValue::Text("Int".into()),
                    ]),
                    CborValue::Null,
                ])]),
                CborValue::Array(vec![CborValue::Array(vec![
                    CborValue::Integer(7.into()),
                    CborValue::Text("M.result".into()),
                    CborValue::Integer(0.into()),
                    CborValue::Text("Text".into()),
                    CborValue::Array(vec![]),
                    CborValue::Array(vec![]),
                    CborValue::Array(vec![]),
                    CborValue::Null,
                    CborValue::Array(vec![]),
                    CborValue::Null,
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
                assert_eq!(bound[0].tier, ValueTier::ForceData);
                assert_eq!(bound[0].host_authority, None);
                assert_eq!(
                    bound[0].root_head.as_ref().map(|head| (
                        head.unit.as_str(),
                        head.module.as_str(),
                        head.name.as_str(),
                    )),
                    Some(("main", "GHC.Types", "Int")),
                );
                assert_eq!(
                    asks,
                    vec![YieldSite {
                        reply_declaration: None,
                        request_type_signatures: None,
                        site: 7,
                        origin: "M.result".into(),
                        ordinal: 0,
                        ty: "Text".into(),
                        modules: Vec::new(),
                        heads: Vec::new(),
                        inputs: Vec::new(),
                        input_type_witnesses: Vec::new(),
                    }]
                );
                assert!(wrapped_source.contains("result ="));
            }
            other => panic!("expected Bind, got {other:?}"),
        }
    }

    #[test]
    fn bound_binder_authority_is_closed_and_required_on_the_new_wire_shape() {
        let fields = vec![
            CborValue::Text("host".into()),
            CborValue::Integer(1.into()),
            CborValue::Text("Tidepool.Session.Val.G1".into()),
            CborValue::Text("ForceData".into()),
            CborValue::Text("Tidepool.Aeson.Value.Value".into()),
            CborValue::Array(vec![
                CborValue::Text("main".into()),
                CborValue::Text("Tidepool.Aeson.Value".into()),
                CborValue::Text("Value".into()),
            ]),
            CborValue::Text("JsonValue".into()),
        ];
        let binder = decode_bound_binder(&CborValue::Array(fields.clone())).unwrap();
        assert_eq!(binder.host_authority, Some(HostBindingAuthority::JsonValue));

        let old_shape = CborValue::Array(fields[..6].to_vec());
        assert!(decode_bound_binder(&old_shape).is_err());

        let mut unknown = fields;
        unknown[6] = CborValue::Text("SameSpellingWrongUnit".into());
        assert!(decode_bound_binder(&CborValue::Array(unknown)).is_err());
    }

    #[test]
    fn yield_site_preserves_captured_reply_declaration_and_refuses_legacy_sites() {
        let mut fields = vec![
            CborValue::Integer(7.into()),
            CborValue::Text("M.request".into()),
            CborValue::Integer(0.into()),
            CborValue::Text("Report".into()),
            CborValue::Array(vec![]),
            CborValue::Array(vec![]),
            CborValue::Array(vec![]),
        ];
        assert!(decode_asks(&CborValue::Array(vec![CborValue::Array(fields.clone())])).is_err());
        fields.push(CborValue::Text("data Report = Report Int".into()));
        assert!(decode_asks(&CborValue::Array(vec![CborValue::Array(fields.clone())])).is_err());
        fields.push(CborValue::Array(vec![]));
        assert!(decode_asks(&CborValue::Array(vec![CborValue::Array(fields.clone())])).is_err());
        fields.push(CborValue::Null);
        assert_eq!(
            decode_asks(&CborValue::Array(vec![CborValue::Array(fields.clone())]))
                .unwrap()
                .remove(0)
                .reply_declaration
                .as_deref(),
            Some("data Report = Report Int")
        );
        fields[7] = CborValue::Integer(1.into());
        assert!(decode_asks(&CborValue::Array(vec![CborValue::Array(fields)])).is_err());
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

    /// `PREPARED_SCAFFOLD_TARGET` names the compile target on the Rust side;
    /// `Tidepool.Session.preparedScaffoldTargetName` must name the exact same
    /// target on the Haskell side, or a compiled scaffold silently targets
    /// the wrong binding. Parses the Haskell literal (never a substring
    /// search) so a rename on either side fails this test.
    #[test]
    fn prepared_scaffold_target_matches_the_haskell_scaffold_name() {
        let session_hs = include_str!("../../../../bridge/haskell/src/Tidepool/Session.hs");
        let mut found = None;
        for line in session_hs.lines() {
            let Some(rest) = line.trim_start().strip_prefix("preparedScaffoldTargetName") else {
                continue;
            };
            let Some(rest) = rest.trim_start().strip_prefix('=') else {
                continue;
            };
            let Some(rest) = rest.trim_start().strip_prefix('"') else {
                continue;
            };
            if let Some(end) = rest.find('"') {
                found = Some(rest[..end].to_string());
                break;
            }
        }
        let found = found.expect("preparedScaffoldTargetName = \"...\" not found in Session.hs");
        assert_eq!(found, PREPARED_SCAFFOLD_TARGET);
    }
}

#[cfg(test)]
mod compiler_packet_replay {
    //! Manual compiler-only qualification of a privately retained host-carrier offer.
    //! Source and recipe hashes are diagnostic inputs. The parser, runtime admission,
    //! compiler endpoint and product issuers create every new authority below.

    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use serde::Deserialize;
    use sha2::{Digest, Sha256};
    use tidepool_codegen::scope::ScopeId;
    use tidepool_toolchain::checked_cell::CheckedCellSpecification;

    use super::{compile_cell_program_admitted, CellCheckRequest, TemplateSelector, TurnTemplate};
    use crate::session::{
        HostBindingType, ModuleEnv, PersistentSession, ResidentSession, RuntimeCompileInputs,
        SessionLib,
    };

    #[derive(Deserialize)]
    struct RetainedFile {
        path: PathBuf,
        sha256: String,
    }

    impl RetainedFile {
        fn bytes(&self) -> Vec<u8> {
            let bytes = std::fs::read(&self.path).unwrap();
            assert_eq!(
                hex(&Sha256::digest(&bytes)),
                self.sha256,
                "{}",
                self.path.display()
            );
            bytes
        }

        fn source(&self) -> String {
            String::from_utf8(self.bytes()).unwrap()
        }
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "lowercase")]
    enum TemplateKind {
        Decl,
        Bind,
        Binddiscard,
        Expr,
    }

    impl TemplateKind {
        fn selector(&self) -> TemplateSelector {
            match self {
                Self::Decl => TemplateSelector::Decl,
                Self::Bind => TemplateSelector::Bind,
                Self::Binddiscard => TemplateSelector::BindDiscard,
                Self::Expr => TemplateSelector::Expr,
            }
        }
    }

    #[derive(Deserialize)]
    struct RetainedTemplate {
        kind: TemplateKind,
        file: RetainedFile,
    }

    #[derive(Deserialize)]
    struct ReplayInputs {
        cell: RetainedFile,
        template: RetainedFile,
        turn_templates: Vec<RetainedTemplate>,
        include: Vec<PathBuf>,
        declared_sources: Vec<RetainedFile>,
        value_generation_before_reservation: u64,
    }

    #[derive(Clone)]
    struct NoExecutionOutput;

    impl crate::session::OutputSink for NoExecutionOutput {
        fn drain(&self) -> Vec<String> {
            Vec::new()
        }

        fn snapshot(&self) -> Vec<String> {
            Vec::new()
        }
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    /// This manual evidence consumer requires the recorded source inventory restored
    /// at its original paths. It neither reuses old compiler proofs nor executes the
    /// command or the compiled placeholder. Automatic host-carrier tests remain the
    /// ordinary qualification path.
    #[test]
    #[ignore = "requires a private authenticated source capsule and a matched compiler"]
    fn retained_command_job_carrier_reissues_compiler_products_without_execution() {
        tidepool_testing::eval_harness::require_extract();
        let manifest = std::env::var_os("TIDEPOOL_HOST_CARRIER_REPLAY_INPUTS").unwrap();
        let manifest_bytes = std::fs::read(manifest).unwrap();
        let inputs: ReplayInputs = serde_json::from_slice(&manifest_bytes).unwrap();
        assert!(inputs
            .include
            .iter()
            .all(|path| path.is_absolute() && path.is_dir()));
        for file in &inputs.declared_sources {
            file.bytes();
        }
        let templates = inputs
            .turn_templates
            .iter()
            .map(|template| TurnTemplate {
                kind: template.kind.selector(),
                source: template.file.source(),
            })
            .collect::<Vec<_>>();
        let specification = Arc::new(CheckedCellSpecification {
            admission_digest: [0; 32],
            cell_source: inputs.cell.source(),
            template_source: inputs.template.source(),
            turn_templates: templates
                .iter()
                .map(|template| {
                    (
                        template.kind.wire_name().to_owned(),
                        template.source.clone(),
                    )
                })
                .collect(),
            injected_modules: Vec::new(),
            reserved_declaration_modules: Vec::new(),
        });
        let plan =
            tidepool_toolchain::artifacts::parse_cell_plan(specification.clone(), &inputs.include)
                .unwrap();
        assert_eq!(plan.items().len(), 1);
        assert_eq!(plan.items()[0].binders(), ["job1"]);
        let root = tempfile::tempdir().unwrap();
        let library = SessionLib::open(
            tidepool_repr::SessionId(202),
            root.path(),
            ModuleEnv::standalone_default(),
        )
        .unwrap()
        .with_validation_include(inputs.include.clone());
        let state = PersistentSession::new(Some(library), 1024 * 1024);
        let mut session =
            ResidentSession::from_persistent_for_test(frunk::HNil, NoExecutionOutput, state);
        session.set_val_gen(tidepool_repr::Generation(
            inputs.value_generation_before_reservation,
        ));
        let admission = session
            .admit_host_carrier_cell_in(
                ScopeId::ROOT,
                plan,
                specification.clone(),
                specification.specification_digest(),
                Sha256::digest(&manifest_bytes).into(),
                inputs.include.clone(),
                "job1".into(),
                HostBindingType::COMMAND_JOB,
                Some(RuntimeCompileInputs::new(None, Vec::new()).unwrap()),
            )
            .unwrap();
        assert_eq!(
            admission.host_carrier(),
            Some(("job1", HostBindingType::COMMAND_JOB))
        );
        let view = admission.view();
        let include = inputs
            .include
            .iter()
            .map(PathBuf::as_path)
            .collect::<Vec<&Path>>();
        let (checked, program) = compile_cell_program_admitted(
            CellCheckRequest {
                exact_context: view.exact_compile_context(),
                session_id: Some(view.session()),
                cell_text: &specification.cell_source,
                template: &specification.template_source,
                include: &include,
                session_root: view.session_root(),
                inject_modules: &[],
                compile_generation: view.next_value_generation().0,
                compile_view_evidence: "",
            },
            admission.clone(),
            &templates,
        )
        .unwrap();
        assert_eq!(checked.items.len(), 1);
        assert_eq!(program.items().len(), 1);
        let item = &program.items()[0];
        let native = item
            .native()
            .expect("new Haskell products passed Rust native admission");
        let certificate = native.value_interface_certificate().unwrap();
        assert_eq!(
            certificate.owner(),
            tidepool_repr::SessionModule::val(view.next_value_generation())
        );
        assert_eq!(native.item(), item.checked_item());
        assert_eq!(native.generation(), view.next_value_generation().0);
        native
            .validate_runtime_admission(item.checked_item().admission_digest(), admission.digest())
            .unwrap();
        assert!(!item.native_products().unwrap().recovery_products.is_empty());
        assert!(!certificate.bytes_owned().is_empty());
        assert!(session.current_binding_in(ScopeId::ROOT, "job1").is_none());
        assert_eq!(session.parked_count(), 0);
    }
}
