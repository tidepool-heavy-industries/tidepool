//! Compiler-query verbs over the actor's current compile view.
//!
//! Translation and projection for the six `Introspection` verbs added beside
//! `info`/`typeOf`. Every answer is relative to one immutable view: the view is
//! captured before the compiler runs and compared again before resumption, and
//! a moved view is refused with `ScopeChanged` rather than reinterpreted.
//!
//! Provenance is that compile view's own triple: scope, publication generation
//! and content fingerprint. It is the only view identity this layer can
//! observe, because `ActorCompileView` exposes no compiler-world handle, so a
//! handler must never synthesize one. Comparing the full triple is what makes
//! `ScopeChanged` meaningful; correlating it with world-publication identity is
//! a runtime-owner follow-up. A caller may pin the view it expects: `Nothing`
//! observes the entry view, while `Just p` is refused at entry when `p` is no
//! longer current. Every answer is also rechecked before the reply, so a view
//! that moves during a query is refused whether or not the request was pinned.
//!
//! Enumeration maps to the existing browse queries. `currentScopeView` never
//! enters the compiler path; the remaining verbs reuse the admitted inspection
//! batch already owned by the structured-introspection path instead of
//! starting a second compiler attempt.
//!
//! `rowFit` is the one verb whose plan depends on the held session: a local
//! binder (empty defining module) and a reference into a session module this
//! view still carries both resolve in the executing scope, while a reference
//! into a generation this view has replaced is answered `FitUnknown` from
//! held state. It must not fall through to an enumerated browse, which would
//! read whatever binding now holds the same spelling.

use std::path::PathBuf;
use std::sync::Arc;

use tidepool_bridge::FromHaskell;
use tidepool_bridge_derive::FromHaskell as DeriveFromHaskell;
use tidepool_runtime::session::{
    IdentifierNamespace, IdentifierRef, InfoEntry, InspectionAvailability, InspectionResult,
    NameNamespace, NameQuery, NameScope, QueryError, ScopeProvenance,
    TypeMatch as RuntimeTypeMatch, TypeMatchQuality as RuntimeTypeMatchQuality,
};

use super::structured_introspection::{
    visit_introspection_identifier, visit_introspection_provenance,
    visit_introspection_query_error, visit_structural_list,
};
use super::to_haskell::{actor_haskell_int, visit_core, visit_named};
use super::*;

/// One effect result crosses into the guest heap as a single value, so an
/// enumeration answer carries a fixed budget instead of growing with the
/// installed API. The same budget is handed to the extraction request
/// (`ScopeBrowseBounded`/`BrowseBounded`/`TypeSearchBounded`), so the worker
/// stops rendering at the cap rather than shipping an unbounded list that the
/// handler trims after transit.
///
/// Basis: 256 is a presentation/transport budget for one notebook answer, not
/// a measured scope statistic. It is the boundary the provisional limit already
/// used; what was missing is enforcement, which now happens in the worker. The
/// handler tests assert that a 16x-budget scope still projects exactly `limit`
/// entries, so the encoded value cannot grow with the installed API. Calibrate
/// from retained query evidence if the boundary budget moves. The handler still
/// trims defensively so a decoder that ignores the request budget cannot hand
/// an unbounded list to the guest encoder.
pub(super) const QUERY_RESULT_LIMIT: usize = 256;

/// A decoded compiler question. The wire enum lives in
/// `generated::introspection`; this type is the actor's own closed set so
/// planning and projection do not depend on generated spelling.
#[derive(Clone, Debug)]
pub(crate) enum CompilerQuery {
    CurrentScopeView,
    ScopeBindings {
        expected_scope: Option<ScopeProvenance>,
    },
    ModuleExports {
        expected_scope: Option<ScopeProvenance>,
        module: String,
    },
    RowFit {
        expected_scope: Option<ScopeProvenance>,
        identifier: IdentifierRef,
        module: RowFitModule,
    },
    TypeOfExpression {
        expected_scope: Option<ScopeProvenance>,
        expression: String,
    },
    TypeSearch {
        expected_scope: Option<ScopeProvenance>,
        pattern: String,
    },
}

fn same_expected_scope(
    left: &Option<ScopeProvenance>,
    right: &Option<ScopeProvenance>,
) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => left.same_view(right),
        _ => false,
    }
}

/// A pin names a view, not a label: two requests pinned to the same view
/// identity are the same query even when their scope labels differ.
impl PartialEq for CompilerQuery {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::CurrentScopeView, Self::CurrentScopeView) => true,
            (
                Self::ScopeBindings {
                    expected_scope: left,
                },
                Self::ScopeBindings {
                    expected_scope: right,
                },
            ) => same_expected_scope(left, right),
            (
                Self::ModuleExports {
                    expected_scope: left_scope,
                    module: left_module,
                },
                Self::ModuleExports {
                    expected_scope: right_scope,
                    module: right_module,
                },
            ) => same_expected_scope(left_scope, right_scope) && left_module == right_module,
            (
                Self::RowFit {
                    expected_scope: left_scope,
                    identifier: left_identifier,
                    module: left_module,
                },
                Self::RowFit {
                    expected_scope: right_scope,
                    identifier: right_identifier,
                    module: right_module,
                },
            ) => {
                same_expected_scope(left_scope, right_scope)
                    && left_identifier == right_identifier
                    && left_module == right_module
            }
            (
                Self::TypeOfExpression {
                    expected_scope: left_scope,
                    expression: left_expression,
                },
                Self::TypeOfExpression {
                    expected_scope: right_scope,
                    expression: right_expression,
                },
            ) => same_expected_scope(left_scope, right_scope) && left_expression == right_expression,
            (
                Self::TypeSearch {
                    expected_scope: left_scope,
                    pattern: left_pattern,
                },
                Self::TypeSearch {
                    expected_scope: right_scope,
                    pattern: right_pattern,
                },
            ) => same_expected_scope(left_scope, right_scope) && left_pattern == right_pattern,
            _ => false,
        }
    }
}

impl Eq for CompilerQuery {}

impl CompilerQuery {
    /// Admit one `rowFit` reference: parse the wire's defining-module string
    /// exactly once. Every later decision consumes the resolved value.
    pub(super) fn row_fit(
        expected_scope: Option<ScopeProvenance>,
        identifier: IdentifierRef,
    ) -> Self {
        let module = RowFitModule::from_wire(&identifier.module);
        Self::RowFit {
            expected_scope,
            identifier,
            module,
        }
    }

    pub(super) fn operation(&self) -> &'static str {
        match self {
            Self::CurrentScopeView => "currentScopeView",
            Self::ScopeBindings { .. } => "scopeBindings",
            Self::ModuleExports { .. } => "moduleExports",
            Self::RowFit { .. } => "rowFit",
            Self::TypeOfExpression { .. } => "typeOfExpression",
            Self::TypeSearch { .. } => "typeSearch",
        }
    }

    /// The view the caller pinned, if any. `None` asks about whatever view the
    /// actor holds at entry.
    pub(super) fn expected_scope(&self) -> Option<&ScopeProvenance> {
        match self {
            Self::CurrentScopeView => None,
            Self::ScopeBindings { expected_scope }
            | Self::ModuleExports { expected_scope, .. }
            | Self::RowFit { expected_scope, .. }
            | Self::TypeOfExpression { expected_scope, .. }
            | Self::TypeSearch { expected_scope, .. } => expected_scope.as_ref(),
        }
    }

    /// Whether this verb still has a question for the compiler. A `rowFit`
    /// reference into a session module this view no longer carries is
    /// answered `Fit::Unknown` from held state; submitting it to an enumerated
    /// browse would read whatever binding now holds the same spelling.
    pub(super) fn plans_compiler_work(&self, live: &LiveSessionModules) -> bool {
        match self {
            Self::CurrentScopeView => false,
            Self::RowFit { module, .. } => {
                !matches!(row_fit_target(module, live), RowFitTarget::StaleSessionModule)
            }
            _ => true,
        }
    }

    /// The probes compile a generated expression; every other verb asks a
    /// question about the held environment.
    pub(super) fn compiles_probe(&self) -> bool {
        matches!(
            self,
            Self::TypeOfExpression { .. } | Self::TypeSearch { .. }
        )
    }

    /// What this verb needs from the exact view the actor holds. Planning is
    /// pure: it reads the view's live session module identities and never the
    /// compiler's source, so a verb answered from held state can never start a
    /// compiler attempt.
    ///
    /// Enumeration and `rowFit` currently reuse the admitted inspection batch
    /// (`ScopeBrowse`, `Browse`), which compiles its inspection module.
    ///
    /// Ownership split after the held-world enumeration spike
    /// (`compiler-query/handlers/ENUMERATION-SPIKE.md`):
    /// - `scopeBindings` becomes a runtime-owned, in-process read of the
    ///   admitted world's published bindings (no extract call) on
    ///   `feature/compiler-query/world-enum-20261019`. Keep this compile-path
    ///   arm working until that handoff lands.
    /// - `moduleExports` moves to a world query once the compiler service
    ///   exposes exports without a target compile; its answer shape then
    ///   narrows to `NameRef` (no display, availability, or references).
    /// - `rowFit` is NO-GO world-side: availability needs the executing
    ///   actor's effect row, which is not retained, so it stays on the
    ///   solver/compile path.
    ///
    /// `rowFit` reads availability from the browsed entries because the Lookup
    /// excision removes the keyed `ExactName` query.
    pub(super) fn plan(
        &self,
        live: &LiveSessionModules,
        provenance: &ScopeProvenance,
    ) -> CompilerPlan {
        match self {
            Self::CurrentScopeView => {
                CompilerPlan::Answered(CompilerQueryAnswer::ScopeView(provenance.clone()))
            }
            Self::ScopeBindings { .. } => {
                CompilerPlan::Queries(vec![InspectionQuery::ScopeBrowseBounded(
                    QUERY_RESULT_LIMIT,
                )])
            }
            Self::ModuleExports { module, .. } => CompilerPlan::Queries(vec![
                InspectionQuery::BrowseBounded {
                    module: module.clone(),
                    expanded: false,
                    budget: QUERY_RESULT_LIMIT,
                },
            ]),
            Self::RowFit { identifier, module, .. } => match row_fit_target(module, live) {
                RowFitTarget::CurrentScope => {
                    CompilerPlan::Queries(vec![InspectionQuery::ScopeBrowse])
                }
                RowFitTarget::Module(module) => CompilerPlan::Queries(vec![
                    InspectionQuery::Browse {
                        module: module.as_str().to_owned(),
                        expanded: true,
                    },
                ]),
                RowFitTarget::StaleSessionModule => CompilerPlan::Answered(
                    CompilerQueryAnswer::Fit(Fit {
                        availability: FitAvailability::Unknown,
                        identifier: identifier.clone(),
                        provenance: provenance.clone(),
                    }),
                ),
            },
            Self::TypeOfExpression { expression, .. } => {
                CompilerPlan::Queries(vec![InspectionQuery::TypeOf(expression.clone())])
            }
            Self::TypeSearch { pattern, .. } => {
                CompilerPlan::Queries(vec![InspectionQuery::TypeSearchBounded {
                    pattern: pattern.clone(),
                    budget: QUERY_RESULT_LIMIT,
                }])
            }
        }
    }
}

/// The session modules an exact view can still name.
///
/// `library` is the declaration module this view imports. `injected_values`
/// includes every value module the compile loads, retained shadowed
/// generations among them, because an already-compiled reference may still be
/// loaded through them. Both come from the runtime's own compile view rather
/// than from re-deriving a generation: `next_value_generation` is the
/// generation a *future* compile would mint, so it is one past the live
/// declaration module and says nothing about which value generations remain
/// loadable.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct LiveSessionModules {
    library: Option<tidepool_repr::SessionModule>,
    injected_values: Vec<tidepool_repr::SessionModule>,
}

impl LiveSessionModules {
    fn of(view: &tidepool_runtime::session::SessionCompileView) -> Self {
        Self {
            library: view.library(),
            injected_values: view.injected_values().to_vec(),
        }
    }

    #[cfg(test)]
    fn fixture(
        library: Option<tidepool_repr::SessionModule>,
        injected_values: Vec<tidepool_repr::SessionModule>,
    ) -> Self {
        Self {
            library,
            injected_values,
        }
    }

    fn serves(&self, module: tidepool_repr::SessionModule) -> bool {
        match module.kind {
            tidepool_repr::SessionModuleKind::Lib => self.library == Some(module),
            tidepool_repr::SessionModuleKind::Val => self.injected_values.contains(&module),
        }
    }
}

/// What one verb needs from the compiler. `Answered` is decided by the held
/// view alone; `Queries` is one admitted inspection batch.
pub(super) enum CompilerPlan {
    Answered(CompilerQueryAnswer),
    Queries(Vec<InspectionQuery>),
}

/// A `rowFit` reference's defining module, resolved once from the wire's
/// `module: String`. The wire shape stays a string; only this boundary decides
/// what the spelling means.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DefiningModule {
    /// The executing scope: an empty wire module names a local binder.
    Local,
    /// A parseable `Tidepool.Session.*` module. Whether this view still carries
    /// that exact generation is decided against the held view.
    Session(tidepool_repr::SessionModule),
    /// A public module name. Reserved session spellings cannot be constructed.
    Public(PublicModuleName),
}

/// A public module name that is not a reserved session spelling.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PublicModuleName(String);

impl PublicModuleName {
    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

/// A reserved `Tidepool.Session.*` spelling that is not a parseable session
/// module. It names no module that can exist, so it is a stale session
/// reference rather than a public module to browse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct UnparseableSessionModule;

/// The admitted form of a `rowFit` reference's defining module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RowFitModule {
    Defining(DefiningModule),
    UnparseableSession(UnparseableSessionModule),
}

impl RowFitModule {
    fn from_wire(module: &str) -> Self {
        match DefiningModule::from_wire(module) {
            Ok(defining) => Self::Defining(defining),
            Err(unparseable) => Self::UnparseableSession(unparseable),
        }
    }

    /// The public module named by this reference, if any.
    fn public_module(&self) -> Option<&PublicModuleName> {
        match self {
            Self::Defining(DefiningModule::Public(module)) => Some(module),
            Self::Defining(DefiningModule::Local | DefiningModule::Session(_))
            | Self::UnparseableSession(_) => None,
        }
    }
}

impl DefiningModule {
    fn from_wire(module: &str) -> Result<Self, UnparseableSessionModule> {
        if module.is_empty() {
            return Ok(Self::Local);
        }
        match tidepool_repr::SessionModule::from_module_name(module) {
            Some(session) => Ok(Self::Session(session)),
            // A reserved spelling that does not parse names no module that
            // can exist; it must never fall through to a public browse.
            None if tidepool_repr::SessionModule::is_reserved_name(module) => {
                Err(UnparseableSessionModule)
            }
            None => Ok(Self::Public(PublicModuleName(module.to_owned()))),
        }
    }
}

/// Where a `rowFit` reference resolves.
#[derive(Clone, Debug, PartialEq, Eq)]
enum RowFitTarget {
    /// A binding in the executing scope: no defining module, or a module this
    /// view still carries.
    CurrentScope,
    /// A public module's exports.
    Module(PublicModuleName),
    /// A session module this view no longer carries. Its spelling may be
    /// reused by a newer generation, so the reference must not read the
    /// current binding of the same name.
    StaleSessionModule,
}

/// Row fit asks about an identifier's availability under the executing row.
/// Every `InfoEntry` carries `thingAvailability`, which calls
/// `signatureAvailability` for values and reads the solved row for other
/// declarations, so browsing the name's owner yields typed availability.
///
/// A reference with no defining module is a local binder: the compiler
/// publishes those with an empty module (`Introspection.hs`), and their
/// identity is `(name, namespace)` in the executing scope. A reference into a
/// session module resolves the same way only while this view still carries
/// that exact generation. Any other reference names its defining public module
/// explicitly. The Lookup excision removes `InspectionQuery::ExactName`; until
/// a keyed, non-enumerating availability query lands, this reads the browsed
/// entries.
///
/// This is the one deliberately unbounded enumeration in the verb set. A cap
/// can drop the requested identifier from the retained prefix and turn a real
/// answer into `FitUnknown`, so `rowFit` keeps the complete module read even
/// though its answer crosses the wire as a single `Fit` record. The world-side
/// enumeration spike ruled a non-compiling `rowFit` out; replacing this with a
/// keyed availability query is the follow-up that removes the last unbounded
/// compiler read.
fn row_fit_target(module: &RowFitModule, live: &LiveSessionModules) -> RowFitTarget {
    match module {
        RowFitModule::Defining(DefiningModule::Local) => RowFitTarget::CurrentScope,
        RowFitModule::Defining(DefiningModule::Session(session)) if live.serves(*session) => {
            RowFitTarget::CurrentScope
        }
        RowFitModule::Defining(DefiningModule::Session(_))
        | RowFitModule::UnparseableSession(_) => RowFitTarget::StaleSessionModule,
        RowFitModule::Defining(DefiningModule::Public(module)) => {
            RowFitTarget::Module(module.clone())
        }
    }
}

/// The compile-view identity an answer is relative to.
///
/// Reuse the structured-introspection identity so `currentScopeView`, `info`,
/// and `typeOf` name the same snapshot for one view. The triple (scope,
/// generation, content fingerprint) is the only identity the actor can observe:
/// `ActorCompileView` has no world/publication accessor, and synthesizing a
/// `compiler-world:` fingerprint would fabricate one. Correlating this triple
/// with world-publication identity is a separate runtime-owner follow-up.
fn current_scope_provenance(
    compile_view: &crate::ActorCompileView,
    source: &ActorWorkbenchSource,
    scope: NameScope,
) -> ScopeProvenance {
    super::structured_provenance(compile_view, source, scope)
}

/// Whether a callable's effects fit the inspecting actor's row. Mirrors the
/// protocol `FitAvailability` constructors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FitAvailability {
    Available,
    Polymorphic,
    Unavailable,
    Unknown,
}

impl From<InspectionAvailability> for FitAvailability {
    fn from(availability: InspectionAvailability) -> Self {
        match availability {
            InspectionAvailability::Available => Self::Available,
            InspectionAvailability::Polymorphic => Self::Polymorphic,
            InspectionAvailability::Unavailable => Self::Unavailable,
            InspectionAvailability::Unknown => Self::Unknown,
        }
    }
}

/// One `rowFit` answer: the measured availability together with the identifier
/// it describes and the compile view it was measured in. The view is the entry
/// snapshot, never a re-read after the query.
impl PartialEq for Fit {
    fn eq(&self, other: &Self) -> bool {
        self.availability == other.availability
            && self.identifier == other.identifier
            && self.provenance.same_view(&other.provenance)
    }
}

impl Eq for Fit {}

#[derive(Clone, Debug)]
pub(super) struct Fit {
    availability: FitAvailability,
    identifier: IdentifierRef,
    provenance: ScopeProvenance,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct NameEntry {
    name: String,
    module: Option<String>,
    kind: String,
    display: String,
    availability: FitAvailability,
    references: Vec<IdentifierRef>,
    reference: IdentifierRef,
}

impl From<InfoEntry> for NameEntry {
    fn from(entry: InfoEntry) -> Self {
        Self {
            name: entry.name,
            module: entry.module,
            kind: entry.kind,
            display: entry.display,
            availability: entry.availability.into(),
            references: entry.references,
            reference: entry.identity,
        }
    }
}

impl PartialEq for NameList {
    fn eq(&self, other: &Self) -> bool {
        self.entries == other.entries
            && self.observed == other.observed
            && self.limit == other.limit
            && self.provenance.same_view(&other.provenance)
    }
}

impl Eq for NameList {}

#[derive(Clone, Debug)]
pub(super) struct NameList {
    entries: Vec<NameEntry>,
    observed: usize,
    limit: usize,
    provenance: ScopeProvenance,
}

impl NameList {
    fn from_entries(entries: Vec<InfoEntry>, provenance: ScopeProvenance) -> Self {
        // The worker deduplicates candidate names and then bounds the encoded
        // list at `QUERY_RESULT_LIMIT`, so a conforming decode carries at most
        // that many distinct entries. `observed` is the retained count, not a
        // scope total: `observed == limit` means "at least limit" and the true
        // scope size is never counted.
        //
        // A longer decode means the worker ignored its budget. Report the cap
        // rather than the decoded length so the wire never carries a count the
        // actor cannot justify; the retained entries are still trimmed below.
        let observed = entries.len().min(QUERY_RESULT_LIMIT);
        let entries = entries
            .into_iter()
            .take(QUERY_RESULT_LIMIT)
            .map(NameEntry::from)
            .collect();
        Self {
            entries,
            observed,
            limit: QUERY_RESULT_LIMIT,
            provenance,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TypeMatchQuality {
    Exact,
    Usable,
}

impl From<RuntimeTypeMatchQuality> for TypeMatchQuality {
    fn from(quality: RuntimeTypeMatchQuality) -> Self {
        match quality {
            RuntimeTypeMatchQuality::Exact => Self::Exact,
            RuntimeTypeMatchQuality::Usable => Self::Usable,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TypeMatch {
    name: String,
    module: Option<String>,
    signature: String,
    quality: TypeMatchQuality,
    availability: FitAvailability,
    references: Vec<IdentifierRef>,
    reference: IdentifierRef,
}

impl From<RuntimeTypeMatch> for TypeMatch {
    fn from(found: RuntimeTypeMatch) -> Self {
        Self {
            name: found.name,
            module: found.module,
            signature: found.signature,
            quality: found.quality.into(),
            availability: found.availability.into(),
            references: found.references,
            reference: found.identity,
        }
    }
}

impl PartialEq for TypeMatches {
    fn eq(&self, other: &Self) -> bool {
        self.matches == other.matches
            && self.observed == other.observed
            && self.limit == other.limit
            && self.provenance.same_view(&other.provenance)
    }
}

impl Eq for TypeMatches {}

#[derive(Clone, Debug)]
pub(super) struct TypeMatches {
    matches: Vec<TypeMatch>,
    observed: usize,
    limit: usize,
    provenance: ScopeProvenance,
}

impl TypeMatches {
    fn from_matches(matches: Vec<RuntimeTypeMatch>, provenance: ScopeProvenance) -> Self {
        // The worker deduplicates candidates and then bounds the encoded
        // match list at the requested budget, so a conforming decode carries
        // at most `QUERY_RESULT_LIMIT` distinct matches. `observed == limit`
        // means "at least limit"; an over-long decode is capped at the budget
        // so no unsupported total crosses the wire.
        let observed = matches.len().min(QUERY_RESULT_LIMIT);
        let matches = matches
            .into_iter()
            .take(QUERY_RESULT_LIMIT)
            .map(TypeMatch::from)
            .collect();
        Self {
            matches,
            observed,
            limit: QUERY_RESULT_LIMIT,
            provenance,
        }
    }
}

impl PartialEq for ExpressionType {
    fn eq(&self, other: &Self) -> bool {
        self.source == other.source
            && self.display == other.display
            && self.availability == other.availability
            && self.provenance.same_view(&other.provenance)
    }
}

impl Eq for ExpressionType {}

#[derive(Clone, Debug)]
pub(super) struct ExpressionType {
    source: String,
    display: String,
    availability: FitAvailability,
    provenance: ScopeProvenance,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "wire-complete severity; probe rejection currently reports errors only"
)]
pub(super) enum DiagnosticSeverity {
    Error,
    Warning,
}

/// A concrete GHC span. Probe compilation currently reports rendered text
/// without a usable location, so every diagnostic this module constructs has
/// no span; the wire shape must still be able to carry one.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "wire-complete span; the compiled rejection carries text only"
)]
pub(super) struct DiagnosticSpan {
    file: String,
    start_line: u32,
    start_col: u32,
    end_line: u32,
    end_col: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Diagnostic {
    span: Option<DiagnosticSpan>,
    severity: DiagnosticSeverity,
    message: String,
}

impl Diagnostic {
    fn error(message: String) -> Self {
        Self {
            span: None,
            severity: DiagnosticSeverity::Error,
            message,
        }
    }
}

/// The actor-side answer; projected to `Either QueryError _` exactly like the
/// existing structured introspection answers.
#[derive(Clone)]
pub(super) enum CompilerQueryAnswer {
    ScopeView(ScopeProvenance),
    Names(NameList),
    Fit(Fit),
    Expression(ExpressionType),
    Matches(TypeMatches),
    QueryError(QueryError),
    ScopeChanged {
        before: ScopeProvenance,
        after: ScopeProvenance,
    },
    ExpressionRejected(Vec<Diagnostic>),
    CompilerUnavailable(String),
}

/// The admitted batch outcome. A user-Haskell failure carries the
/// classification message; infrastructure failures keep their own variant so a
/// probe that never compiled is not reported as an expression rejection.
pub(super) enum BatchOutcome {
    Answer(Result<InspectionResult, String>),
    Infrastructure(String),
}

/// A pinned view that no longer matches the entry snapshot is refused before
/// any compiler work. `before` is the view the caller pinned; `after` is the
/// view the actor actually holds at entry.
pub(super) fn entry_scope_refusal(
    query: &CompilerQuery,
    entry: &ScopeProvenance,
) -> Option<CompilerQueryAnswer> {
    query.expected_scope().and_then(|expected| {
        (!expected.same_view(entry)).then(|| CompilerQueryAnswer::ScopeChanged {
            before: expected.clone(),
            after: entry.clone(),
        })
    })
}

/// Project one verb's answer. Staleness is decided first: a pinned view that
/// never matched at entry, or a view that moved while the query ran, is a
/// refusal, never an empty or stale success.
pub(super) fn compiler_query_answer(
    query: &CompilerQuery,
    before: ScopeProvenance,
    current: ScopeProvenance,
    outcome: BatchOutcome,
) -> CompilerQueryAnswer {
    if matches!(query, CompilerQuery::CurrentScopeView) {
        return CompilerQueryAnswer::ScopeView(before);
    }
    if let Some(refusal) = entry_scope_refusal(query, &before) {
        return refusal;
    }
    if !before.same_view(&current) {
        return CompilerQueryAnswer::ScopeChanged {
            before,
            after: current,
        };
    }
    let result = match outcome {
        BatchOutcome::Infrastructure(detail) => {
            return CompilerQueryAnswer::CompilerUnavailable(detail)
        }
        BatchOutcome::Answer(Err(detail)) if query.compiles_probe() => {
            return CompilerQueryAnswer::ExpressionRejected(vec![Diagnostic::error(detail)])
        }
        BatchOutcome::Answer(Err(detail)) => {
            return CompilerQueryAnswer::CompilerUnavailable(detail)
        }
        BatchOutcome::Answer(Ok(result)) => result,
    };
    match (query, result) {
        (CompilerQuery::ScopeBindings { .. }, InspectionResult::Browse { entries, .. })
        | (CompilerQuery::ModuleExports { .. }, InspectionResult::Browse { entries, .. }) => {
            enumerated_names(entries, before)
        }
        (CompilerQuery::RowFit { identifier, .. }, InspectionResult::Browse { entries, .. }) => {
            CompilerQueryAnswer::Fit(Fit {
                availability: fit_from_entries(identifier, &entries),
                identifier: identifier.clone(),
                provenance: before,
            })
        }
        (
            CompilerQuery::TypeOfExpression { .. },
            InspectionResult::Type {
                expression,
                display,
                availability,
            },
        ) => CompilerQueryAnswer::Expression(ExpressionType {
            source: expression,
            display,
            availability: availability.into(),
            provenance: before,
        }),
        (CompilerQuery::TypeOfExpression { .. }, InspectionResult::Rejected { diagnostic })
        | (CompilerQuery::TypeSearch { .. }, InspectionResult::Rejected { diagnostic }) => {
            CompilerQueryAnswer::ExpressionRejected(vec![Diagnostic::error(diagnostic)])
        }
        (CompilerQuery::TypeSearch { .. }, InspectionResult::TypeMatches { matches, .. }) => {
            CompilerQueryAnswer::Matches(TypeMatches::from_matches(matches, before))
        }
        (_, InspectionResult::NotFound { query: missing }) => CompilerQueryAnswer::QueryError(
            QueryError::Unknown(refusal_query(query, &before, &missing)),
        ),
        (_, InspectionResult::Ambiguous { query: ambiguous, entries }) => {
            let candidates = entries.into_iter().map(|entry| entry.identity).collect();
            CompilerQueryAnswer::QueryError(QueryError::Ambiguous(
                refusal_query(query, &before, &ambiguous),
                candidates,
            ))
        }
        (
            CompilerQuery::ModuleExports { .. } | CompilerQuery::RowFit { .. },
            InspectionResult::ModuleNotFound { module, .. },
        ) => {
            CompilerQueryAnswer::QueryError(QueryError::UnknownModule(module))
        }
        (_, result) => CompilerQueryAnswer::CompilerUnavailable(format!(
            "unexpected compiler-query result: {}",
            result.render()
        )),
    }
}

/// The identity of the question an `Unknown`/`Ambiguous` refusal answers. The
/// enumeration and probe queries do not carry a `NameQuery`, so reconstruct
/// only the fields the error payload needs from the request itself.
/// The single construction site for an enumeration answer. `moduleExports`
/// narrows to `NameRef` once the world-query path lands; only this function
/// and the wire type change then, not each verb arm or its tests.
fn enumerated_names(entries: Vec<InfoEntry>, provenance: ScopeProvenance) -> CompilerQueryAnswer {
    CompilerQueryAnswer::Names(NameList::from_entries(entries, provenance))
}

fn refusal_query(query: &CompilerQuery, before: &ScopeProvenance, name: &str) -> NameQuery {
    let scope = match query {
        CompilerQuery::ModuleExports { module, .. } => NameScope::PublicModule(module.clone()),
        // An empty module is a local binder, and a reserved session module is
        // private: neither is a public module scope to name in the refusal.
        CompilerQuery::RowFit { module, .. } => match module.public_module() {
            Some(module) => NameScope::PublicModule(module.as_str().to_owned()),
            None => before.scope.clone(),
        },
        _ => before.scope.clone(),
    };
    let namespace = match query {
        CompilerQuery::RowFit { identifier, .. } => match identifier.namespace {
            IdentifierNamespace::Value => NameNamespace::Value,
            IdentifierNamespace::Type => NameNamespace::Type,
            IdentifierNamespace::Constructor => NameNamespace::Constructor,
            // `NameNamespace` has no field case for record-selector identity.
            // The refusal payload therefore widens to `Any` rather than
            // inventing a wire variant; the typed `IdentifierRef` already
            // carries the lost distinction for programmatic consumers.
            IdentifierNamespace::Field => NameNamespace::Any,
        },
        _ => NameNamespace::Any,
    };
    NameQuery {
        scope,
        namespace,
        name: name.to_owned(),
    }
}

/// Project availability from the entries that can *be* this identifier. A
/// candidate must match the requested identity's namespace and name; its
/// `identity` is the entry's canonical resolved declaration, so a value and a
/// type that share a spelling remain distinct without consulting `kind`.
/// Prefer the requested defining module: a re-export reports the defining
/// module, which legitimately differs from the module the caller named, so an
/// exact module match narrows the candidates but is not required.
///
/// Namespaces never share a spelling space: when no candidate serves the
/// requested namespace, the answer is `Unknown`. Falling back to a same-spelled
/// entry from another namespace would report a value's row fit for a
/// `TypeIdentifier`, or the reverse.
///
/// References are not identity: `infoReferences` are the entry's parent and
/// component references, not the entry itself, so a reference hit can name a
/// related declaration. When several candidates remain, only report a fit they
/// all agree on; a convenient first row cannot stand in for a row observation.
/// Absence means the executing environment did not expose the identifier —
/// report `Unknown`, never `Unavailable`.
fn fit_from_entries(identifier: &IdentifierRef, entries: &[InfoEntry]) -> FitAvailability {
    let candidates = entries
        .iter()
        .filter(|entry| {
            entry.identity.name == identifier.name
                && entry.identity.namespace == identifier.namespace
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return FitAvailability::Unknown;
    }
    let defining = candidates
        .iter()
        .copied()
        .filter(|entry| entry.identity.module == identifier.module)
        .collect::<Vec<_>>();
    let candidates = if defining.is_empty() {
        candidates
    } else {
        defining
    };
    let mut fits = candidates
        .iter()
        .map(|entry| FitAvailability::from(entry.availability));
    let first = fits.next().expect("candidates is nonempty");
    if fits.all(|fit| fit == first) {
        first
    } else {
        FitAvailability::Unknown
    }
}


impl tidepool_bridge::sealed::ToHaskellSealed for CompilerQueryAnswer {}

impl ToHaskell for CompilerQueryAnswer {
    fn visit(
        &self,
        table: &DataConTable,
        visitor: &mut dyn HaskellVisitor,
    ) -> Result<(), BridgeError> {
        let right = matches!(
            self,
            Self::ScopeView(_)
                | Self::Names(_)
                | Self::Fit(_)
                | Self::Expression(_)
                | Self::Matches(_)
        );
        visit_named(
            table,
            visitor,
            "Data.Either",
            if right { "Right" } else { "Left" },
            |visitor| match self {
                Self::ScopeView(provenance) => {
                    visit_introspection_provenance(table, visitor, provenance)
                }
                Self::Names(names) => visit_name_list(table, visitor, names),
                Self::Fit(fit) => visit_fit(table, visitor, fit),
                Self::Expression(expression) => visit_expression_type(table, visitor, expression),
                Self::Matches(matches) => visit_type_matches(table, visitor, matches),
                Self::QueryError(error) => visit_introspection_query_error(table, visitor, error),
                Self::ScopeChanged { before, after } => {
                    visit_core(table, visitor, "ScopeChanged", |visitor| {
                        visit_introspection_provenance(table, visitor, before)?;
                        visit_introspection_provenance(table, visitor, after)
                    })
                }
                Self::ExpressionRejected(diagnostics) => {
                    visit_core(table, visitor, "ExpressionRejected", |visitor| {
                        visit_structural_list(table, visitor, diagnostics, |diagnostic, visitor| {
                            visit_diagnostic(table, visitor, diagnostic)
                        })
                    })
                }
                Self::CompilerUnavailable(detail) => {
                    visit_core(table, visitor, "CompilerUnavailable", |visitor| {
                        detail.visit(table, visitor)
                    })
                }
            },
        )
    }
}

fn visit_fit(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    fit: &Fit,
) -> Result<(), BridgeError> {
    visit_core(table, visitor, "Fit", |visitor| {
        visit_fit_availability(table, visitor, fit.availability)?;
        visit_introspection_identifier(table, visitor, &fit.identifier)?;
        visit_introspection_provenance(table, visitor, &fit.provenance)
    })
}

fn visit_fit_availability(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    availability: FitAvailability,
) -> Result<(), BridgeError> {
    visit_core(
        table,
        visitor,
        match availability {
            FitAvailability::Available => "FitAvailable",
            FitAvailability::Polymorphic => "FitPolymorphic",
            FitAvailability::Unavailable => "FitUnavailable",
            FitAvailability::Unknown => "FitUnknown",
        },
        |_| Ok(()),
    )
}

fn visit_optional_text(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    value: Option<&String>,
) -> Result<(), BridgeError> {
    match value {
        Some(value) => visit_named(table, visitor, "GHC.Maybe", "Just", |visitor| {
            value.visit(table, visitor)
        }),
        None => visit_named(table, visitor, "GHC.Maybe", "Nothing", |_| Ok(())),
    }
}

fn visit_name_entry(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    entry: &NameEntry,
) -> Result<(), BridgeError> {
    visit_core(table, visitor, "NameEntry", |visitor| {
        entry.name.visit(table, visitor)?;
        visit_optional_text(table, visitor, entry.module.as_ref())?;
        entry.kind.visit(table, visitor)?;
        entry.display.visit(table, visitor)?;
        visit_fit_availability(table, visitor, entry.availability)?;
        visit_structural_list(table, visitor, &entry.references, |reference, visitor| {
            visit_introspection_identifier(table, visitor, reference)
        })?;
        visit_introspection_identifier(table, visitor, &entry.reference)
    })
}

fn visit_name_list(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    names: &NameList,
) -> Result<(), BridgeError> {
    visit_core(table, visitor, "NameList", |visitor| {
        visit_structural_list(table, visitor, &names.entries, |entry, visitor| {
            visit_name_entry(table, visitor, entry)
        })?;
        actor_haskell_int(names.observed as u64, "observed name count")?.visit(table, visitor)?;
        actor_haskell_int(names.limit as u64, "name list limit")?.visit(table, visitor)?;
        visit_introspection_provenance(table, visitor, &names.provenance)
    })
}

fn visit_expression_type(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    expression: &ExpressionType,
) -> Result<(), BridgeError> {
    visit_core(table, visitor, "ExpressionType", |visitor| {
        expression.source.visit(table, visitor)?;
        expression.display.visit(table, visitor)?;
        visit_fit_availability(table, visitor, expression.availability)?;
        visit_introspection_provenance(table, visitor, &expression.provenance)
    })
}

fn visit_type_match(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    found: &TypeMatch,
) -> Result<(), BridgeError> {
    visit_core(table, visitor, "TypeMatch", |visitor| {
        found.name.visit(table, visitor)?;
        visit_optional_text(table, visitor, found.module.as_ref())?;
        found.signature.visit(table, visitor)?;
        visit_core(
            table,
            visitor,
            match found.quality {
                TypeMatchQuality::Exact => "TypeMatchExact",
                TypeMatchQuality::Usable => "TypeMatchUsable",
            },
            |_| Ok(()),
        )?;
        visit_fit_availability(table, visitor, found.availability)?;
        visit_structural_list(table, visitor, &found.references, |reference, visitor| {
            visit_introspection_identifier(table, visitor, reference)
        })?;
        visit_introspection_identifier(table, visitor, &found.reference)
    })
}

fn visit_type_matches(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    matches: &TypeMatches,
) -> Result<(), BridgeError> {
    visit_core(table, visitor, "TypeMatches", |visitor| {
        visit_structural_list(table, visitor, &matches.matches, |found, visitor| {
            visit_type_match(table, visitor, found)
        })?;
        actor_haskell_int(matches.observed as u64, "observed type match count")?
            .visit(table, visitor)?;
        actor_haskell_int(matches.limit as u64, "type match limit")?.visit(table, visitor)?;
        visit_introspection_provenance(table, visitor, &matches.provenance)
    })
}

fn visit_diagnostic(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    diagnostic: &Diagnostic,
) -> Result<(), BridgeError> {
    visit_core(table, visitor, "Diagnostic", |visitor| {
        match &diagnostic.span {
            Some(span) => visit_named(table, visitor, "GHC.Maybe", "Just", |visitor| {
                visit_diagnostic_span(table, visitor, span)
            })?,
            None => visit_named(table, visitor, "GHC.Maybe", "Nothing", |_| Ok(()))?,
        }
        visit_core(
            table,
            visitor,
            match diagnostic.severity {
                DiagnosticSeverity::Error => "DiagnosticError",
                DiagnosticSeverity::Warning => "DiagnosticWarning",
            },
            |_| Ok(()),
        )?;
        diagnostic.message.visit(table, visitor)
    })
}

fn visit_diagnostic_span(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    span: &DiagnosticSpan,
) -> Result<(), BridgeError> {
    visit_core(table, visitor, "DiagnosticSpan", |visitor| {
        span.file.visit(table, visitor)?;
        for (value, label) in [
            (span.start_line, "diagnostic start line"),
            (span.start_col, "diagnostic start column"),
            (span.end_line, "diagnostic end line"),
            (span.end_col, "diagnostic end column"),
        ] {
            actor_haskell_int(u64::from(value), label)?.visit(table, visitor)?;
        }
        Ok(())
    })
}

/// Decode the wire `IdentifierRef` carried by `rowFit`.
#[derive(DeriveFromHaskell)]
#[haskell(name = "IdentifierRef")]
pub(super) struct IntrospectionIdentifier {
    module: String,
    name: String,
    namespace: IntrospectionIdentifierNamespace,
}

#[derive(DeriveFromHaskell)]
#[allow(
    clippy::enum_variant_names,
    reason = "variant names are the wire truth: FromHaskell matches them \
              verbatim against Haskell constructor names"
)]
pub(super) enum IntrospectionIdentifierNamespace {
    ValueIdentifier,
    TypeIdentifier,
    ConstructorIdentifier,
    FieldIdentifier,
}

impl From<IntrospectionIdentifier> for IdentifierRef {
    fn from(identifier: IntrospectionIdentifier) -> Self {
        Self {
            module: identifier.module,
            name: identifier.name,
            namespace: match identifier.namespace {
                IntrospectionIdentifierNamespace::ValueIdentifier => {
                    tidepool_runtime::session::IdentifierNamespace::Value
                }
                IntrospectionIdentifierNamespace::TypeIdentifier => {
                    tidepool_runtime::session::IdentifierNamespace::Type
                }
                IntrospectionIdentifierNamespace::ConstructorIdentifier => {
                    tidepool_runtime::session::IdentifierNamespace::Constructor
                }
                IntrospectionIdentifierNamespace::FieldIdentifier => {
                    tidepool_runtime::session::IdentifierNamespace::Field
                }
            },
        }
    }
}

/// Decode the `Maybe ScopeProvenance` a request pins.
pub(super) fn expected_scope_from_value(
    value: &HaskellValue,
    table: &DataConTable,
) -> Result<Option<ScopeProvenance>, BridgeError> {
    Option::<IntrospectionProvenance>::from_value(value, table)
        .map(|provenance| provenance.map(ScopeProvenance::from))
}

/// The scope generation's nominal Haskell type is not settled across the
/// surfaces this value crosses: the effect schema declares `Int`, the
/// hand-written `Tidepool.Introspection` record says `Word64`, and the
/// bridge's own `u64` decoder only unwraps `Word`'s `W#`. A caller may also
/// pass back a provenance this actor previously emitted, which the structured
/// answer path encodes with `actor_haskell_int` (an `I#`). Accept every boxed
/// spelling the bridge can observe — `I#`, `W#`, and `W64#` — and narrow to
/// the runtime's `u64` after decoding. Remove the tolerance once the schema
/// and the Haskell record agree.
struct GenerationWord(u64);

impl tidepool_bridge::sealed::FromHaskellSealed for GenerationWord {}

impl FromHaskell for GenerationWord {
    fn from_value(value: &HaskellValue, table: &DataConTable) -> Result<Self, BridgeError> {
        if let Ok(word) = u64::from_value(value, table) {
            return Ok(Self(word));
        }
        if let Ok(int) = i64::from_value(value, table) {
            return u64::try_from(int)
                .map(Self)
                .map_err(|_| tidepool_bridge::type_mismatch("non-negative scope generation", value));
        }
        if let Some(payload) = word64_payload(value, table) {
            return u64::from_value(payload, table).map(Self);
        }
        Err(tidepool_bridge::type_mismatch(
            "Int, Word, or Word64 scope generation",
            value,
        ))
    }
}

/// The `Word#` inside a boxed `Word64` (`W64#`); `None` for every other
/// shape. Kept separate so the decoder reads as three deliberate attempts
/// rather than nested conditionals.
fn word64_payload<'a>(
    value: &'a HaskellValue,
    table: &DataConTable,
) -> Option<&'a HaskellValue> {
    match value {
        HaskellValue::Con(id, fields)
            if table.name_of(*id) == Some("W64#") && fields.len() == 1 =>
        {
            Some(&fields[0])
        }
        _ => None,
    }
}

/// Decode the wire `ScopeProvenance` a request pins. Positional, matching the
/// Haskell record order `scope, generation, fingerprint`.
#[derive(DeriveFromHaskell)]
#[haskell(name = "ScopeProvenance")]
pub(super) struct IntrospectionProvenance {
    provenance_scope: IntrospectionNameScope,
    #[haskell(hs = "provenanceGeneration", hs_type = "Int")]
    provenance_generation: GenerationWord,
    provenance_fingerprint: String,
}

#[derive(DeriveFromHaskell)]
#[allow(
    clippy::enum_variant_names,
    reason = "variant names are the wire truth: FromHaskell matches them \
              verbatim against Haskell constructor names"
)]
pub(super) enum IntrospectionNameScope {
    CurrentScope,
    PublicModule(String),
}

impl From<IntrospectionProvenance> for ScopeProvenance {
    fn from(provenance: IntrospectionProvenance) -> Self {
        Self {
            scope: match provenance.provenance_scope {
                IntrospectionNameScope::CurrentScope => NameScope::Current,
                IntrospectionNameScope::PublicModule(module) => NameScope::PublicModule(module),
            },
            generation: provenance.provenance_generation.0,
            fingerprint: provenance.provenance_fingerprint,
        }
    }
}

impl<H, O> ResidentActorWorkbench<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    /// Service one compiler-query verb without holding the resident machine
    /// while the compiler runs. The suspended continuation remains the actor's
    /// exclusive turn, mirroring `resume_structured_introspection`.
    ///
    /// `currentScopeView` returns the entry snapshot and never re-reads it. A
    /// publication that lands between admission and reply is therefore
    /// detectable only by a later pinned request, not by this answer.
    pub(crate) async fn resume_compiler_query(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        query: CompilerQuery,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        let source = self.access.source.config.clone();
        let snapshot_source = source.clone();
        let query_for_snapshot = query.clone();
        let snapshot = self
            .access
            .with_machine(context.clone(), move |session, context, _| {
                let view = actor_compile_view(session, context, &snapshot_source)?;
                let provenance =
                    current_scope_provenance(&view, &snapshot_source, NameScope::Current);
                let live = LiveSessionModules::of(view.session_view());
                let plan = query_for_snapshot.plan(&live, &provenance);
                // A pinned view that already moved is refused before any
                // compiler input is captured; `currentScopeView` and a stale
                // session reference need no inputs because the held view
                // answers them.
                let refused = entry_scope_refusal(&query_for_snapshot, &provenance).is_some();
                let inputs = (!refused && matches!(plan, CompilerPlan::Queries(_))).then(|| {
                    session
                        .capture_inspection_inputs(view.session_view())
                        .map_err(|error| error.to_string())
                });
                Ok((view, provenance, plan, inputs))
            })
            .await?;
        let (compile_view, provenance, plan, inputs) = snapshot;

        if let Some(refusal) = entry_scope_refusal(&query, &provenance) {
            return self
                .access
                .with_continuation_machine(context, move |session, _, _, settlement| {
                    session
                        .resume_classified(hole, refusal, settlement)
                        .map_err(classify_resumption)
                })
                .await;
        }

        let queries = match plan {
            CompilerPlan::Answered(answer) => {
                return self
                    .access
                    .with_continuation_machine(
                        context,
                        move |session, _, _, settlement| {
                            session
                                .resume_classified(hole, answer, settlement)
                                .map_err(classify_resumption)
                        },
                    )
                    .await;
            }
            CompilerPlan::Queries(queries) => queries,
        };

        let compiler_source = source.clone();
        let effects = context.haskell_effects_alias.clone();
        self.access
            .log_include_roots_if_changed(context.placement.session, context.source_layer.as_ref());
        let inspection_span = tracing::info_span!(
            "compile_blocking",
            actor = %context.actor,
            session = %context.placement.session,
            operation = query.operation(),
        );
        let compiler_work =
            crate::capabilities::CompilerOwnerScope::current().and_then(|scope| {
                scope
                    .with_source_dependency(context.source_dependency())
                    .admit_work()
            })?;
        let mut cancel_on_drop = CancelCompilerTransactionOnDrop(Some(compiler_work.control()));
        let spawn = {
            let _entered = inspection_span.enter();
            spawn_blocking_in_span(move || {
                compiler_work.run(|settlement| {
                    let inputs = match inputs {
                        Some(Ok(inputs)) => inputs,
                        Some(Err(detail)) => return BatchOutcome::Infrastructure(detail),
                        None => {
                            return BatchOutcome::Infrastructure(
                                "compiler query admitted without inspection inputs".into(),
                            )
                        }
                    };
                    let prepared = compiler_source.prepare(&compile_view);
                    let include = prepared
                        .include
                        .iter()
                        .map(PathBuf::as_path)
                        .collect::<Vec<_>>();
                    match run_admitted_inspections(
                        InspectionRequest {
                            exact_context: compile_view.exact_compile_context(),
                            preamble: &prepared.preamble,
                            imports: &prepared.imports,
                            include: &include,
                            session_root: compile_view.session_root(),
                            inject_modules: &prepared.injected,
                            queries: &queries,
                            effects: Some(&effects),
                        },
                        compile_view.session_view(),
                        &inputs,
                        settlement,
                    ) {
                        Ok(mut results) if results.len() == queries.len() => {
                            BatchOutcome::Answer(Ok(results.remove(0)))
                        }
                        Ok(results) => BatchOutcome::Infrastructure(format!(
                            "inspection returned {} results for {} queries",
                            results.len(),
                            queries.len()
                        )),
                        Err(error)
                            if classify_compile(&error).class == FailureClass::UserHaskell =>
                        {
                            BatchOutcome::Answer(Err(classify_compile(&error).message))
                        }
                        Err(error) => BatchOutcome::Infrastructure(error.to_string()),
                    }
                })
            })
        };
        let inspected = crate::call_timing::timed_compile(spawn)
            .await
            .map_err(ResidentActorWorkbenchError::Join)?;
        cancel_on_drop.0 = None;

        self.access
            .with_continuation_machine(context, move |session, context, _, settlement| {
                let current_view = actor_compile_view(session, context, &source)?;
                let current = current_scope_provenance(&current_view, &source, NameScope::Current);
                let answer = compiler_query_answer(&query, provenance, current, inspected);
                session
                    .resume_classified(hole, answer, settlement)
                    .map_err(classify_resumption)
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::SessionModule;

    fn provenance(generation: u64, fingerprint: &str) -> ScopeProvenance {
        ScopeProvenance {
            scope: NameScope::Current,
            generation,
            fingerprint: fingerprint.into(),
        }
    }

    fn identifier(module: &str, name: &str) -> IdentifierRef {
        IdentifierRef {
            module: module.into(),
            name: name.into(),
            namespace: IdentifierNamespace::Value,
        }
    }

    fn row_fit_module(module: &str) -> RowFitModule {
        RowFitModule::from_wire(module)
    }

    fn typed_identifier(namespace: IdentifierNamespace) -> IdentifierRef {
        IdentifierRef {
            module: "Data.Thing".into(),
            name: "Thing".into(),
            namespace,
        }
    }

    /// One held view: declaration module `Lib.G4`, with `Val.G3` retained for
    /// references compiled before the last generation.
    fn live() -> LiveSessionModules {
        LiveSessionModules::fixture(
            Some(SessionModule::lib(tidepool_repr::Generation(4))),
            vec![
                SessionModule::val(tidepool_repr::Generation(3)),
                SessionModule::val(tidepool_repr::Generation(4)),
            ],
        )
    }

    /// Test-only projection of the pure plan against one representative
    /// view, so a verb's queries can be asserted without a live machine.
    trait Planned {
        fn inspection_queries(&self) -> Vec<InspectionQuery>;
        fn compiler_work_planned(&self) -> bool;
    }

    impl Planned for CompilerQuery {
        fn inspection_queries(&self) -> Vec<InspectionQuery> {
            match planned(self) {
                CompilerPlan::Queries(queries) => queries,
                // `currentScopeView` is answered from the held view and
                // submits nothing, exactly as it did before planning became
                // explicit.
                CompilerPlan::Answered(CompilerQueryAnswer::ScopeView(_)) => Vec::new(),
                CompilerPlan::Answered(_) => {
                    panic!("expected compiler queries from {}", self.operation())
                }
            }
        }

        fn compiler_work_planned(&self) -> bool {
            CompilerQuery::plans_compiler_work(self, &live())
        }
    }

    fn planned(query: &CompilerQuery) -> CompilerPlan {
        query.plan(&live(), &provenance(4, "world"))
    }

    fn entry(name: &str, module: &str, availability: InspectionAvailability) -> InfoEntry {
        entry_with(
            name,
            module,
            "value",
            IdentifierNamespace::Value,
            availability,
        )
    }

    fn entry_with(
        name: &str,
        module: &str,
        kind: &str,
        namespace: IdentifierNamespace,
        availability: InspectionAvailability,
    ) -> InfoEntry {
        InfoEntry {
            name: name.into(),
            module: Some(module.into()),
            kind: kind.into(),
            display: format!("{name} :: Int"),
            availability,
            references: Vec::new(),
            identity: IdentifierRef {
                module: module.into(),
                name: name.into(),
                namespace,
            },
        }
    }

    fn browse(entries: Vec<InfoEntry>) -> InspectionResult {
        InspectionResult::Browse {
            module: String::new(),
            expanded: true,
            entries,
        }
    }

    fn compiler_verbs_with(expected: Option<ScopeProvenance>) -> Vec<CompilerQuery> {
        vec![
            CompilerQuery::ScopeBindings {
                expected_scope: expected.clone(),
            },
            CompilerQuery::ModuleExports {
                expected_scope: expected.clone(),
                module: "Data.Thing".into(),
            },
            CompilerQuery::row_fit(expected.clone(), identifier("Data.Thing", "thing")),
            CompilerQuery::TypeOfExpression {
                expected_scope: expected.clone(),
                expression: "1 + 1".into(),
            },
            CompilerQuery::TypeSearch {
                expected_scope: expected,
                pattern: "Int -> Int".into(),
            },
        ]
    }

    fn compiler_verbs() -> Vec<CompilerQuery> {
        compiler_verbs_with(None)
    }

    fn successful_result(query: &CompilerQuery) -> InspectionResult {
        match query {
            CompilerQuery::CurrentScopeView => unreachable!("no compiler result"),
            CompilerQuery::ScopeBindings { .. }
            | CompilerQuery::ModuleExports { .. }
            | CompilerQuery::RowFit { .. } => browse(vec![entry(
                "thing",
                "Data.Thing",
                InspectionAvailability::Available,
            )]),
            CompilerQuery::TypeOfExpression { expression, .. } => InspectionResult::Type {
                expression: expression.clone(),
                display: "Int".into(),
                availability: InspectionAvailability::Available,
            },
            CompilerQuery::TypeSearch { .. } => InspectionResult::TypeMatches {
                query: "Int -> Int".into(),
                matches: vec![RuntimeTypeMatch {
                    name: "succ".into(),
                    module: Some("Prelude".into()),
                    signature: "Int -> Int".into(),
                    quality: RuntimeTypeMatchQuality::Exact,
                    availability: InspectionAvailability::Available,
                    references: vec![identifier("Prelude", "succ")],
                    identity: identifier("Prelude", "succ"),
                }],
            },
        }
    }

    #[test]
    fn translation_maps_each_verb_to_its_owner_query() {
        assert!(CompilerQuery::CurrentScopeView
            .inspection_queries()
            .is_empty());
        assert_eq!(
            CompilerQuery::ScopeBindings {
                expected_scope: None
            }
            .inspection_queries(),
            vec![InspectionQuery::ScopeBrowseBounded(QUERY_RESULT_LIMIT)]
        );
        assert_eq!(
            CompilerQuery::ModuleExports {
                expected_scope: None,
                module: "Data.Thing".into()
            }
            .inspection_queries(),
            vec![InspectionQuery::BrowseBounded {
                module: "Data.Thing".into(),
                expanded: false,
                budget: QUERY_RESULT_LIMIT,
            }]
        );
        assert_eq!(
            CompilerQuery::row_fit(None, identifier("Data.Thing", "thing"))
            .inspection_queries(),
            vec![InspectionQuery::Browse {
                module: "Data.Thing".into(),
                expanded: true
            }]
        );
        assert_eq!(
            CompilerQuery::row_fit(None, identifier("Tidepool.Session.Val.G3", "x"))
            .inspection_queries(),
            vec![InspectionQuery::ScopeBrowse]
        );
        assert_eq!(
            CompilerQuery::TypeOfExpression {
                expected_scope: None,
                expression: "1 + 1".into()
            }
            .inspection_queries(),
            vec![InspectionQuery::TypeOf("1 + 1".into())]
        );
        assert_eq!(
            CompilerQuery::TypeSearch {
                expected_scope: None,
                pattern: "a -> a".into()
            }
            .inspection_queries(),
            vec![InspectionQuery::TypeSearchBounded {
                pattern: "a -> a".into(),
                budget: QUERY_RESULT_LIMIT,
            }]
        );
    }

    #[test]
    fn enumeration_verbs_request_the_extraction_budget() {
        // The cap has to reach the worker: the handler asks for the bounded
        // question instead of trimming an unbounded answer after transit.
        assert_eq!(
            CompilerQuery::ScopeBindings {
                expected_scope: None
            }
            .inspection_queries(),
            vec![InspectionQuery::ScopeBrowseBounded(QUERY_RESULT_LIMIT)]
        );
        assert_eq!(
            CompilerQuery::ModuleExports {
                expected_scope: None,
                module: "Data.Thing".into()
            }
            .inspection_queries(),
            vec![InspectionQuery::BrowseBounded {
                module: "Data.Thing".into(),
                expanded: false,
                budget: QUERY_RESULT_LIMIT,
            }]
        );
        assert_eq!(
            CompilerQuery::TypeSearch {
                expected_scope: None,
                pattern: "a -> a".into()
            }
            .inspection_queries(),
            vec![InspectionQuery::TypeSearchBounded {
                pattern: "a -> a".into(),
                budget: QUERY_RESULT_LIMIT,
            }]
        );
    }

    #[test]
    fn row_fit_keeps_the_complete_module_read() {
        // A budget can drop the asked-for identifier from the retained prefix,
        // turning a real availability answer into `FitUnknown`. `rowFit` is the
        // documented exception: its wire answer is one `Fit`, so it keeps the
        // unbounded enumeration until a keyed availability query lands.
        assert_eq!(
            CompilerQuery::row_fit(None, identifier("Data.Thing", "thing"))
            .inspection_queries(),
            vec![InspectionQuery::Browse {
                module: "Data.Thing".into(),
                expanded: true,
            }]
        );
        assert_eq!(
            CompilerQuery::row_fit(None, identifier("Tidepool.Session.Val.G3", "x"))
            .inspection_queries(),
            vec![InspectionQuery::ScopeBrowse]
        );
    }

    #[test]
    fn row_fit_resolves_local_and_live_session_references() {
        let current_scope = |identifier| {
            CompilerQuery::row_fit(None, identifier)
            .inspection_queries()
        };
        // A binder with no defining module is published as the empty module.
        assert_eq!(
            current_scope(identifier("", "local")),
            vec![InspectionQuery::ScopeBrowse]
        );
        // The live declaration generation is this view's own scope.
        assert_eq!(
            current_scope(identifier("Tidepool.Session.Lib.G4", "local")),
            vec![InspectionQuery::ScopeBrowse]
        );
        // A shadowed value generation is still loadable, so it still resolves.
        assert_eq!(
            current_scope(identifier("Tidepool.Session.Val.G3", "local")),
            vec![InspectionQuery::ScopeBrowse]
        );
        // Public modules keep their keyed owner read.
        assert_eq!(
            current_scope(identifier("Data.Thing", "thing")),
            vec![InspectionQuery::Browse {
                module: "Data.Thing".into(),
                expanded: true,
            }]
        );
    }

    #[test]
    fn stale_session_reference_answers_unknown_without_compiler_work() {
        let view = provenance(4, "world");
        for stale in [
            // A declaration module this view has replaced.
            "Tidepool.Session.Lib.G3",
            // A value module this view no longer injects.
            "Tidepool.Session.Val.G2",
            // A reserved spelling that names no module at all.
            "Tidepool.Session.Lib.G",
        ] {
            let query = CompilerQuery::row_fit(None, identifier(stale, "local"));
            assert!(
                !query.plans_compiler_work(&live()),
                "{stale}: a stale reference must not enumerate the current scope"
            );
            match query.plan(&live(), &view) {
                CompilerPlan::Answered(CompilerQueryAnswer::Fit(fit)) => {
                    assert_eq!(fit.availability, FitAvailability::Unknown, "{stale}");
                    assert_eq!(fit.identifier, identifier(stale, "local"), "{stale}");
                    assert!(fit.provenance.same_view(&view), "{stale}");
                }
                CompilerPlan::Answered(_) => {
                    panic!("{stale}: expected a fit answer, got another held-view answer")
                }
                CompilerPlan::Queries(_) => {
                    panic!("{stale}: stale session reference planned compiler work")
                }
            }
        }
        // The namespace is never widened to the current scope's same spelling:
        // a type query cannot read a value's availability.
        let asked = typed_identifier(IdentifierNamespace::Type);
        let query = CompilerQuery::row_fit(None, IdentifierRef {
                module: "Tidepool.Session.Lib.G3".into(),
                ..asked
            });
        assert!(!query.plans_compiler_work(&live()));
    }

    #[test]
    fn row_fit_targets_classify_every_reference_shape() {
        let view = live();
        // Empty defining module: a local binder in the executing scope.
        assert!(matches!(
            row_fit_target(&row_fit_module(""), &view),
            RowFitTarget::CurrentScope
        ));
        // The live declaration generation and a retained value generation.
        assert!(matches!(
            row_fit_target(&row_fit_module("Tidepool.Session.Lib.G4"), &view),
            RowFitTarget::CurrentScope
        ));
        assert!(matches!(
            row_fit_target(&row_fit_module("Tidepool.Session.Val.G3"), &view),
            RowFitTarget::CurrentScope
        ));
        // A replaced declaration generation and a reserved spelling that names
        // no module: held-state unknown, never a current-scope read.
        assert!(matches!(
            row_fit_target(&row_fit_module("Tidepool.Session.Lib.G3"), &view),
            RowFitTarget::StaleSessionModule
        ));
        assert!(matches!(
            row_fit_target(&row_fit_module("Tidepool.Session.Lib.G"), &view),
            RowFitTarget::StaleSessionModule
        ));
        // A public module keeps its keyed owner read.
        assert!(
            matches!(row_fit_target(&row_fit_module("Data.Thing"), &view),
                RowFitTarget::Module(module) if module.as_str() == "Data.Thing")
        );
    }

    #[test]
    fn row_fit_reference_admits_exactly_one_module_shape() {
        assert_eq!(DefiningModule::from_wire(""), Ok(DefiningModule::Local));
        assert_eq!(
            DefiningModule::from_wire("Tidepool.Session.Lib.G4"),
            Ok(DefiningModule::Session(
                SessionModule::from_module_name("Tidepool.Session.Lib.G4").unwrap()
            ))
        );
        assert!(matches!(
            DefiningModule::from_wire("Data.Thing"),
            Ok(DefiningModule::Public(module)) if module.as_str() == "Data.Thing"
        ));
        // Reserved spellings that name no module never become public modules.
        for spelling in ["Tidepool.Session.", "Tidepool.Session.Lib.G"] {
            assert_eq!(
                DefiningModule::from_wire(spelling),
                Err(UnparseableSessionModule),
                "{spelling}"
            );
            assert_eq!(
                row_fit_target(&row_fit_module(spelling), &live()),
                RowFitTarget::StaleSessionModule,
                "{spelling}"
            );
        }
    }

    #[test]
    fn row_fit_refusals_name_the_executing_scope_for_local_references() {
        let view = provenance(4, "world");
        for module in ["", "Tidepool.Session.Lib.G3"] {
            let query = CompilerQuery::row_fit(None, identifier(module, "local"));
            let refusal = refusal_query(&query, &view, "local");
            assert_eq!(refusal.scope, NameScope::Current, "{module:?}");
            assert_eq!(refusal.namespace, NameNamespace::Value, "{module:?}");
            assert_eq!(refusal.name, "local");
        }
        // Public modules keep their named scope in the refusal payload.
        let query = CompilerQuery::row_fit(None, identifier("Data.Thing", "thing"));
        assert_eq!(
            refusal_query(&query, &view, "thing").scope,
            NameScope::PublicModule("Data.Thing".into())
        );
    }

    #[test]
    fn a_large_scope_cannot_encode_more_than_the_budget() {
        // `visit_name_list` iterates exactly `NameList::entries`, so bounding
        // the retained entries is what bounds the encoded effect result. A
        // decoder that ignores the worker budget must still not hand the guest
        // an unbounded list.
        let view = provenance(1, "world");
        let entries = (0..QUERY_RESULT_LIMIT * 16)
            .map(|index| {
                entry(
                    &format!("thing{index}"),
                    "Data.Thing",
                    InspectionAvailability::Available,
                )
            })
            .collect::<Vec<_>>();
        let answer = compiler_query_answer(
            &CompilerQuery::ScopeBindings {
                expected_scope: None,
            },
            view.clone(),
            view.clone(),
            BatchOutcome::Answer(Ok(browse(entries))),
        );
        match answer {
            CompilerQueryAnswer::Names(names) => {
                assert_eq!(names.entries.len(), QUERY_RESULT_LIMIT);
                assert_eq!(names.limit, QUERY_RESULT_LIMIT);
                // An over-long decode reads as "at least limit", never as a
                // total the actor cannot observe.
                assert_eq!(names.observed, names.limit);
            }
            other => panic!("expected a name list, got {}", answer_name(&other)),
        }

        let matches = (0..QUERY_RESULT_LIMIT * 16)
            .map(|index| RuntimeTypeMatch {
                name: format!("match{index}"),
                module: Some("Prelude".into()),
                signature: "Int -> Int".into(),
                quality: RuntimeTypeMatchQuality::Usable,
                availability: InspectionAvailability::Available,
                references: vec![identifier("Prelude", "succ")],
                identity: identifier("Prelude", "succ"),
            })
            .collect::<Vec<_>>();
        let answer = compiler_query_answer(
            &CompilerQuery::TypeSearch {
                expected_scope: None,
                pattern: "Int -> Int".into(),
            },
            view.clone(),
            view.clone(),
            BatchOutcome::Answer(Ok(InspectionResult::TypeMatches {
                query: "Int -> Int".into(),
                matches,
            })),
        );
        match answer {
            CompilerQueryAnswer::Matches(found) => {
                assert_eq!(found.matches.len(), QUERY_RESULT_LIMIT);
                assert_eq!(found.limit, QUERY_RESULT_LIMIT);
                assert_eq!(found.observed, found.limit);
            }
            other => panic!("expected type matches, got {}", answer_name(&other)),
        }
    }

    #[test]
    fn enumeration_never_plans_a_probe_compile() {
        // The handler never plans `TypeOf`/`TypeSearch` for an enumeration.
        // Runtime-level `inspection.compile_attempts == 0` remains pending:
        // the admitted batch still compiles its inspection module today.
        assert!(!CompilerQuery::CurrentScopeView.compiler_work_planned());
        assert!(CompilerQuery::CurrentScopeView
            .inspection_queries()
            .is_empty());
        for query in compiler_verbs() {
            assert!(query.compiler_work_planned(), "{query:?}");
            if !query.compiles_probe() {
                let planned = query.inspection_queries();
                assert!(
                    planned.iter().all(|planned| matches!(
                        planned,
                        InspectionQuery::ScopeBrowse
                            | InspectionQuery::ScopeBrowseBounded(_)
                            | InspectionQuery::Browse { .. }
                            | InspectionQuery::BrowseBounded { .. }
                    )),
                    "enumeration planned a probe or walk: {query:?} -> {planned:?}"
                );
            } else {
                assert!(
                    query
                        .inspection_queries()
                        .iter()
                        .any(|planned| matches!(
                            planned,
                            InspectionQuery::TypeOf(_)
                                | InspectionQuery::TypeSearch(_)
                                | InspectionQuery::TypeSearchBounded { .. }
                        )),
                    "probe verb planned no probe query: {query:?}"
                );
            }
        }
    }

    #[test]
    fn both_probes_request_only_their_type_question_and_project_no_value() {
        // The admitted inspection batch compiles a probe to obtain its type
        // and never evaluates it (`tidepool/runtime/src/session/inspection.rs`,
        // "Inspect an ordered batch without evaluating it"). Each probe verb
        // must therefore plan exactly its type question, and its answer must
        // carry only the type evidence — never a rendered or evaluated value.
        for (query, planned) in [
            (
                CompilerQuery::TypeOfExpression {
                    expected_scope: None,
                    expression: "1 + 1".into(),
                },
                InspectionQuery::TypeOf("1 + 1".into()),
            ),
            (
                CompilerQuery::TypeSearch {
                    expected_scope: None,
                    pattern: "a -> a".into(),
                },
                InspectionQuery::TypeSearchBounded {
                    pattern: "a -> a".into(),
                    budget: QUERY_RESULT_LIMIT,
                },
            ),
        ] {
            assert!(query.compiles_probe(), "{query:?}");
            assert_eq!(query.inspection_queries(), vec![planned], "{query:?}");

            let answer = compiler_query_answer(
                &query,
                provenance(1, "world"),
                provenance(1, "world"),
                BatchOutcome::Answer(Ok(successful_result(&query))),
            );
            match answer {
                CompilerQueryAnswer::Expression(expression) => {
                    assert_eq!(expression.source, "1 + 1");
                    assert_eq!(expression.display, "Int");
                }
                CompilerQueryAnswer::Matches(matches) => {
                    assert_eq!(matches.matches[0].signature, "Int -> Int");
                }
                other => panic!("expected a type-only answer, got {}", answer_name(&other)),
            }
        }
    }

    #[test]
    fn a_view_that_moves_during_a_query_is_refused() {
        let before = provenance(7, "world-a");
        let after = provenance(8, "world-b");
        for query in compiler_verbs() {
            let answer = compiler_query_answer(
                &query,
                before.clone(),
                after.clone(),
                BatchOutcome::Answer(Ok(successful_result(&query))),
            );
            assert!(
                matches!(
                    answer,
                    CompilerQueryAnswer::ScopeChanged { before: ref seen_before, after: ref seen_after }
                        if seen_before.same_view(&before) && seen_after.same_view(&after)
                ),
                "{query:?} reinterpreted a stale view"
            );
        }
    }

    #[test]
    fn stale_expected_scope_is_refused_at_entry_even_when_the_query_would_succeed() {
        let entry_view = provenance(7, "world-a");
        let stale = provenance(3, "world-old");
        for query in compiler_verbs_with(Some(stale.clone())) {
            let answer = compiler_query_answer(
                &query,
                entry_view.clone(),
                entry_view.clone(),
                BatchOutcome::Answer(Ok(successful_result(&query))),
            );
            assert!(
                matches!(
                    answer,
                    CompilerQueryAnswer::ScopeChanged { before: ref seen_before, after: ref seen_after }
                        if seen_before.same_view(&stale) && seen_after.same_view(&entry_view)
                ),
                "{query:?} accepted a stale pin"
            );
        }
    }

    #[test]
    fn a_stale_pin_wins_over_a_view_that_moved_during_the_query() {
        let stale = provenance(3, "world-old");
        let entry_view = provenance(7, "world-a");
        let after = provenance(9, "world-b");
        for query in compiler_verbs_with(Some(stale.clone())) {
            let answer = compiler_query_answer(
                &query,
                entry_view.clone(),
                after.clone(),
                BatchOutcome::Answer(Ok(successful_result(&query))),
            );
            assert!(
                matches!(
                    answer,
                    CompilerQueryAnswer::ScopeChanged { before: ref seen_before, after: ref seen_after }
                        if seen_before.same_view(&stale) && seen_after.same_view(&entry_view)
                ),
                "{query:?} reported the moved view instead of refusing the stale pin at entry"
            );
        }
    }

    #[test]
    fn a_scope_label_change_alone_is_not_staleness() {
        // The same generation and content can be labelled as the executing
        // scope or as a public module depending on which entry point asked.
        // Only the view identity decides staleness; the label stays in the
        // diagnostic provenance.
        let pinned = ScopeProvenance {
            scope: NameScope::Current,
            generation: 7,
            fingerprint: "world-a".into(),
        };
        let relabelled = ScopeProvenance {
            scope: NameScope::PublicModule("Data.Thing".into()),
            generation: 7,
            fingerprint: "world-a".into(),
        };
        let query = CompilerQuery::ScopeBindings {
            expected_scope: Some(pinned.clone()),
        };
        assert!(entry_scope_refusal(&query, &relabelled).is_none());
        let answer = compiler_query_answer(
            &query,
            pinned.clone(),
            relabelled.clone(),
            BatchOutcome::Answer(Ok(browse(vec![entry(
                "thing",
                "Data.Thing",
                InspectionAvailability::Available,
            )]))),
        );
        assert!(
            matches!(answer, CompilerQueryAnswer::Names(_)),
            "a relabelled view was refused: {}", answer_name(&answer)
        );
        // A real identity change is still refused, with the full provenance.
        let moved = ScopeProvenance {
            generation: 8,
            ..relabelled
        };
        assert!(matches!(
            compiler_query_answer(
                &query,
                pinned.clone(),
                moved.clone(),
                BatchOutcome::Answer(Ok(browse(vec![entry(
                    "thing",
                    "Data.Thing",
                    InspectionAvailability::Available,
                )]))),
            ),
            CompilerQueryAnswer::ScopeChanged { before: ref seen_before, after: ref seen_after }
                if seen_before.same_view(&pinned) && seen_after.same_view(&moved)
        ));
    }

    #[test]
    fn current_scope_view_reports_its_snapshot_without_compiler_work() {
        let before = provenance(7, "world-a");
        let later = provenance(9, "world-b");
        let answer = compiler_query_answer(
            &CompilerQuery::CurrentScopeView,
            before.clone(),
            later,
            BatchOutcome::Infrastructure("must not be consulted".into()),
        );
        assert!(matches!(answer, CompilerQueryAnswer::ScopeView(ref seen) if seen.same_view(&before)));
    }

    #[test]
    fn a_matching_pin_answers_against_the_entry_view() {
        let entry_view = provenance(7, "world-a");
        let requested = identifier("Data.Thing", "thing");
        let answer = compiler_query_answer(
            &CompilerQuery::row_fit(Some(entry_view.clone()), requested.clone()),
            entry_view.clone(),
            entry_view.clone(),
            BatchOutcome::Answer(Ok(browse(vec![entry(
                "thing",
                "Data.Thing",
                InspectionAvailability::Polymorphic,
            )]))),
        );
        match answer {
            CompilerQueryAnswer::Fit(fit) => {
                assert_eq!(fit.availability, FitAvailability::Polymorphic);
                assert_eq!(fit.identifier, requested);
                assert!(fit.provenance.same_view(&entry_view));
            }
            other => panic!("expected a row fit, got {}", answer_name(&other)),
        }
    }

    #[test]
    fn row_fit_projects_observed_availability() {
        let requested = identifier("Data.Thing", "thing");
        for (availability, expected) in [
            (InspectionAvailability::Available, FitAvailability::Available),
            (
                InspectionAvailability::Polymorphic,
                FitAvailability::Polymorphic,
            ),
            (
                InspectionAvailability::Unavailable,
                FitAvailability::Unavailable,
            ),
            (InspectionAvailability::Unknown, FitAvailability::Unknown),
        ] {
            let answer = compiler_query_answer(
                &CompilerQuery::row_fit(None, requested.clone()),
                provenance(1, "world"),
                provenance(1, "world"),
                BatchOutcome::Answer(Ok(browse(vec![entry(
                    "thing",
                    "Data.Thing",
                    availability,
                )]))),
            );
            assert!(
                matches!(answer, CompilerQueryAnswer::Fit(fit) if fit.availability == expected),
                "{availability:?} projected incorrectly"
            );
        }
    }

    #[test]
    fn row_fit_refuses_a_same_spelled_name_from_another_namespace() {
        let answer = compiler_query_answer(
            &CompilerQuery::row_fit(None, typed_identifier(IdentifierNamespace::Type)),
            provenance(1, "world"),
            provenance(1, "world"),
            BatchOutcome::Answer(Ok(browse(vec![entry(
                "Thing",
                "Data.Thing",
                InspectionAvailability::Available,
            )]))),
        );
        assert!(matches!(
            answer,
            CompilerQueryAnswer::Fit(fit)
                if fit.availability == FitAvailability::Unknown
        ));
    }

    #[test]
    fn row_fit_matches_identity_namespace_not_the_rendered_kind() {
        let fit = |kind: &str, identity_namespace: IdentifierNamespace| {
            compiler_query_answer(
                &CompilerQuery::row_fit(None, typed_identifier(IdentifierNamespace::Type)),
                provenance(1, "world"),
                provenance(1, "world"),
                BatchOutcome::Answer(Ok(browse(vec![entry_with(
                    "Thing",
                    "Data.Thing",
                    kind,
                    identity_namespace,
                    InspectionAvailability::Available,
                )]))),
            )
        };

        // A `type`-looking kind whose canonical identity is a value must not
        // answer a type query.
        assert!(matches!(
            fit("type", IdentifierNamespace::Value),
            CompilerQueryAnswer::Fit(ref answer) if answer.availability == FitAvailability::Unknown
        ));
        // Identity is authoritative in the other direction too: a value-shaped
        // kind does not veto a type identity.
        assert!(matches!(
            fit("value", IdentifierNamespace::Type),
            CompilerQueryAnswer::Fit(ref answer)
                if answer.availability == FitAvailability::Available
        ));
    }

    #[test]
    fn row_fit_reports_ambiguity_as_unknown_unless_every_candidate_agrees() {
        let requested = identifier("Data.Thing", "thing");
        let ambiguous = |availabilities: &[InspectionAvailability]| {
            browse(
                availabilities
                    .iter()
                    .map(|availability| entry("thing", "Data.Thing", *availability))
                    .collect(),
            )
        };
        let answer = |result| {
            compiler_query_answer(
                &CompilerQuery::row_fit(None, requested.clone()),
                provenance(1, "world"),
                provenance(1, "world"),
                BatchOutcome::Answer(Ok(result)),
            )
        };

        assert!(matches!(
            answer(ambiguous(&[
                InspectionAvailability::Available,
                InspectionAvailability::Available,
            ])),
            CompilerQueryAnswer::Fit(fit) if fit.availability == FitAvailability::Available
        ));
        assert!(matches!(
            answer(ambiguous(&[
                InspectionAvailability::Available,
                InspectionAvailability::Unavailable,
            ])),
            CompilerQueryAnswer::Fit(fit) if fit.availability == FitAvailability::Unknown
        ));
    }

    #[test]
    fn missing_row_fit_entry_is_unknown_not_unavailable() {
        let answer = compiler_query_answer(
            &CompilerQuery::row_fit(None, identifier("Data.Thing", "absent")),
            provenance(1, "world"),
            provenance(1, "world"),
            BatchOutcome::Answer(Ok(browse(vec![entry(
                "other",
                "Data.Thing",
                InspectionAvailability::Available,
            )]))),
        );
        assert!(matches!(
            answer,
            CompilerQueryAnswer::Fit(fit)
                if fit.availability == FitAvailability::Unknown
        ));
    }

    #[test]
    fn row_fit_reads_the_named_entry_not_an_entry_that_references_it() {
        let constructor = entry_with(
            "MkThing",
            "Data.Thing",
            "constructor",
            IdentifierNamespace::Constructor,
            InspectionAvailability::Available,
        );
        let ty = entry_with(
            "Thing",
            "Data.Thing",
            "type",
            IdentifierNamespace::Type,
            InspectionAvailability::Unavailable,
        );
        let answer = compiler_query_answer(
            &CompilerQuery::row_fit(None, typed_identifier(IdentifierNamespace::Type)),
            provenance(1, "world"),
            provenance(1, "world"),
            BatchOutcome::Answer(Ok(browse(vec![constructor, ty]))),
        );
        assert!(matches!(
            answer,
            CompilerQueryAnswer::Fit(fit)
                if fit.availability == FitAvailability::Unavailable
        ));
    }

    #[test]
    fn row_fit_distinguishes_missing_from_unknown_module() {
        let missing = compiler_query_answer(
            &CompilerQuery::row_fit(None, identifier("Data.Thing", "absent")),
            provenance(1, "world"),
            provenance(1, "world"),
            BatchOutcome::Answer(Ok(browse(Vec::new()))),
        );
        assert!(matches!(
            missing,
            CompilerQueryAnswer::Fit(fit)
                if fit.availability == FitAvailability::Unknown
        ));

        let unknown_module = compiler_query_answer(
            &CompilerQuery::row_fit(None, identifier("Data.Missing", "thing")),
            provenance(1, "world"),
            provenance(1, "world"),
            BatchOutcome::Answer(Ok(InspectionResult::ModuleNotFound {
                module: "Data.Missing".into(),
                scopes: vec![NameScope::PublicModule("Data.Missing".into())],
            })),
        );
        assert!(matches!(
            unknown_module,
            CompilerQueryAnswer::QueryError(QueryError::UnknownModule(ref module))
                if module == "Data.Missing"
        ));
    }

    #[test]
    fn row_fit_accepts_a_reexport_from_the_named_module() {
        let reexport = entry_with(
            "thing",
            "Internal.Thing",
            "value",
            IdentifierNamespace::Value,
            InspectionAvailability::Unavailable,
        );
        let answer = compiler_query_answer(
            &CompilerQuery::row_fit(None, identifier("Data.Thing", "thing")),
            provenance(1, "world"),
            provenance(1, "world"),
            BatchOutcome::Answer(Ok(browse(vec![reexport]))),
        );
        assert!(matches!(
            answer,
            CompilerQueryAnswer::Fit(fit)
                if fit.availability == FitAvailability::Unavailable
        ));
    }

    #[test]
    fn module_not_found_is_a_typed_refusal() {
        let answer = compiler_query_answer(
            &CompilerQuery::ModuleExports {
                expected_scope: None,
                module: "Data.Missing".into(),
            },
            provenance(1, "world"),
            provenance(1, "world"),
            BatchOutcome::Answer(Ok(InspectionResult::ModuleNotFound {
                module: "Data.Missing".into(),
                scopes: Vec::new(),
            })),
        );
        assert!(matches!(
            answer,
            CompilerQueryAnswer::QueryError(QueryError::UnknownModule(ref module))
                if module == "Data.Missing"
        ));
    }

    #[test]
    fn not_found_is_a_typed_unknown_not_infrastructure() {
        let answer = compiler_query_answer(
            &CompilerQuery::ModuleExports {
                expected_scope: None,
                module: "Data.Thing".into(),
            },
            provenance(1, "world"),
            provenance(1, "world"),
            BatchOutcome::Answer(Ok(InspectionResult::NotFound {
                query: "thing".into(),
            })),
        );
        match answer {
            CompilerQueryAnswer::QueryError(QueryError::Unknown(search)) => {
                assert_eq!(search.scope, NameScope::PublicModule("Data.Thing".into()));
                assert_eq!(search.namespace, NameNamespace::Any);
                assert_eq!(search.name, "thing");
            }
            other => panic!("expected a typed unknown, got {}", answer_name(&other)),
        }
    }

    #[test]
    fn ambiguous_is_a_typed_refusal_not_infrastructure() {
        let answer = compiler_query_answer(
            &CompilerQuery::ScopeBindings {
                expected_scope: None,
            },
            provenance(1, "world"),
            provenance(1, "world"),
            BatchOutcome::Answer(Ok(InspectionResult::Ambiguous {
                query: "thing".into(),
                entries: vec![
                    entry("thing", "Data.A", InspectionAvailability::Available),
                    entry("thing", "Data.B", InspectionAvailability::Available),
                ],
            })),
        );
        match answer {
            CompilerQueryAnswer::QueryError(QueryError::Ambiguous(search, candidates)) => {
                assert_eq!(search.scope, NameScope::Current);
                assert_eq!(search.name, "thing");
                assert_eq!(candidates.len(), 2);
                assert_eq!(candidates[0].module, "Data.A");
            }
            other => panic!("expected a typed ambiguity, got {}", answer_name(&other)),
        }
    }

    #[test]
    fn a_rejected_probe_is_a_typed_diagnostic() {
        let answer = compiler_query_answer(
            &CompilerQuery::TypeOfExpression {
                expected_scope: None,
                expression: "1 +".into(),
            },
            provenance(1, "world"),
            provenance(1, "world"),
            BatchOutcome::Answer(Err("parse error on input `+'".into())),
        );
        match answer {
            CompilerQueryAnswer::ExpressionRejected(diagnostics) => {
                assert_eq!(diagnostics.len(), 1);
                assert_eq!(diagnostics[0].severity, DiagnosticSeverity::Error);
                assert!(diagnostics[0].span.is_none());
                assert_eq!(diagnostics[0].message, "parse error on input `+'");
            }
            other => panic!("expected a typed rejection, got {}", answer_name(&other)),
        }
    }

    #[test]
    fn type_of_expression_projects_the_probe_and_its_view() {
        let before = provenance(4, "world");
        let answer = compiler_query_answer(
            &CompilerQuery::TypeOfExpression {
                expected_scope: None,
                expression: "1 + 1".into(),
            },
            before.clone(),
            before.clone(),
            BatchOutcome::Answer(Ok(InspectionResult::Type {
                expression: "1 + 1".into(),
                display: "Int".into(),
                availability: InspectionAvailability::Polymorphic,
            })),
        );
        match answer {
            CompilerQueryAnswer::Expression(expression) => {
                assert_eq!(expression.source, "1 + 1");
                assert_eq!(expression.display, "Int");
                assert_eq!(expression.availability, FitAvailability::Polymorphic);
                assert!(expression.provenance.same_view(&before));
            }
            other => panic!("expected an expression type, got {}", answer_name(&other)),
        }
    }

    #[test]
    fn probe_infrastructure_failure_is_not_an_expression_rejection() {
        let answer = compiler_query_answer(
            &CompilerQuery::TypeSearch {
                expected_scope: None,
                pattern: "a -> a".into(),
            },
            provenance(1, "world"),
            provenance(1, "world"),
            BatchOutcome::Infrastructure("worker exited".into()),
        );
        assert!(matches!(
            answer,
            CompilerQueryAnswer::CompilerUnavailable(ref detail) if detail == "worker exited"
        ));
    }

    #[test]
    fn an_over_long_enumeration_reads_as_at_least_the_budget() {
        let entry_view = provenance(1, "world");
        let entries = (0..QUERY_RESULT_LIMIT + 3)
            .map(|index| {
                entry(
                    &format!("thing{index}"),
                    "Data.Thing",
                    InspectionAvailability::Available,
                )
            })
            .collect::<Vec<_>>();
        let answer = compiler_query_answer(
            &CompilerQuery::ScopeBindings {
                expected_scope: None,
            },
            entry_view.clone(),
            entry_view.clone(),
            BatchOutcome::Answer(Ok(browse(entries))),
        );
        match answer {
            CompilerQueryAnswer::Names(names) => {
                assert_eq!(names.entries.len(), QUERY_RESULT_LIMIT);
                // A conforming worker stops at the budget, so a longer list
                // means the worker ignored it. The wire still reports no more
                // than the budget: "at least limit", never an unjustified total.
                assert_eq!(names.observed, names.limit);
                assert_eq!(names.limit, QUERY_RESULT_LIMIT);
                assert!(names.provenance.same_view(&entry_view));
            }
            other => panic!("expected a name list, got {}", answer_name(&other)),
        }
    }

    #[test]
    fn module_exports_projects_browse_entries() {
        let entry_view = provenance(2, "world");
        let listed = entry("thing", "Data.Thing", InspectionAvailability::Unknown);
        let identity = listed.identity.clone();
        let answer = compiler_query_answer(
            &CompilerQuery::ModuleExports {
                expected_scope: None,
                module: "Data.Thing".into(),
            },
            entry_view.clone(),
            entry_view.clone(),
            BatchOutcome::Answer(Ok(browse(vec![listed]))),
        );
        match answer {
            CompilerQueryAnswer::Names(names) => {
                assert_eq!(names.entries.len(), 1);
                assert_eq!(names.entries[0].reference, identity);
                assert_eq!(names.observed, 1);
                assert_eq!(names.limit, QUERY_RESULT_LIMIT);
                assert!(names.provenance.same_view(&entry_view));
            }
            other => panic!("expected a name list, got {}", answer_name(&other)),
        }
    }

    #[test]
    fn type_search_projects_quality_identity_and_view() {
        let entry_view = provenance(3, "world");
        let found = RuntimeTypeMatch {
            name: "succ".into(),
            module: Some("Prelude".into()),
            signature: "Int -> Int".into(),
            quality: RuntimeTypeMatchQuality::Usable,
            availability: InspectionAvailability::Available,
            references: vec![identifier("Prelude", "succ")],
            identity: identifier("Prelude", "succ"),
        };
        let answer = compiler_query_answer(
            &CompilerQuery::TypeSearch {
                expected_scope: None,
                pattern: "Int -> Int".into(),
            },
            entry_view.clone(),
            entry_view.clone(),
            BatchOutcome::Answer(Ok(InspectionResult::TypeMatches {
                query: "Int -> Int".into(),
                matches: vec![found],
            })),
        );
        match answer {
            CompilerQueryAnswer::Matches(matches) => {
                assert_eq!(matches.observed, 1);
                assert_eq!(matches.limit, QUERY_RESULT_LIMIT);
                assert!(matches.provenance.same_view(&entry_view));
                assert_eq!(matches.matches[0].quality, TypeMatchQuality::Usable);
                assert_eq!(
                    matches.matches[0].availability,
                    FitAvailability::Available
                );
                assert_eq!(
                    matches.matches[0].reference,
                    identifier("Prelude", "succ")
                );
            }
            other => panic!("expected type matches, got {}", answer_name(&other)),
        }
    }

    #[test]
    fn reserved_session_prefix_matches_the_haskell_owner() {
        // `bridge/haskell/src/Tidepool/Session.hs` defines
        // `isReservedSessionModuleName = isPrefixOf "Tidepool.Session."`.
        // `SessionModule::is_reserved_name` is the Rust owner of that spelling;
        // this pin fails if either boundary drifts. The exact string
        // `Tidepool.Session` (no trailing dot) is not reserved on either side.
        assert!(SessionModule::is_reserved_name("Tidepool.Session."));
        assert!(SessionModule::is_reserved_name("Tidepool.Session.Val.G1"));
        assert!(SessionModule::is_reserved_name("Tidepool.Session.Lib.G12"));
        assert!(!SessionModule::is_reserved_name("Tidepool.Session"));
        assert!(!SessionModule::is_reserved_name("Tidepool.SessionX.Val.G1"));
    }

    fn answer_name(answer: &CompilerQueryAnswer) -> &'static str {
        match answer {
            CompilerQueryAnswer::ScopeView(_) => "scope view",
            CompilerQueryAnswer::Names(_) => "names",
            CompilerQueryAnswer::Fit(_) => "fit",
            CompilerQueryAnswer::Expression(_) => "expression",
            CompilerQueryAnswer::Matches(_) => "matches",
            CompilerQueryAnswer::QueryError(_) => "query error",
            CompilerQueryAnswer::ScopeChanged { .. } => "scope changed",
            CompilerQueryAnswer::ExpressionRejected(_) => "expression rejected",
            CompilerQueryAnswer::CompilerUnavailable(_) => "compiler unavailable",
        }
    }
}
