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

use std::path::PathBuf;
use std::sync::Arc;

use tidepool_bridge::FromHaskell;
use tidepool_bridge_derive::DeriveFromHaskell;
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

/// One effect result crosses into the guest heap as a single value. Keep the
/// returned rows bounded and report the observed count beside the applied
/// limit, so truncation is visible instead of being mistaken for the whole
/// scope. Calibrate from retained query evidence if the boundary budget moves.
/// The decoder collects the complete list first; counting happens before this
/// limit is applied, never from the retained prefix.
pub(super) const QUERY_RESULT_LIMIT: usize = 256;

/// A decoded compiler question. The wire enum lives in
/// `generated::introspection`; this type is the actor's own closed set so
/// planning and projection do not depend on generated spelling.
#[derive(Clone, Debug, PartialEq, Eq)]
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

impl CompilerQuery {
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

    /// `currentScopeView` reads the snapshot the caller already holds; it must
    /// never admit compiler work.
    pub(super) fn plans_compiler_work(&self) -> bool {
        !matches!(self, Self::CurrentScopeView)
    }

    /// The probes compile a generated expression; every other verb asks a
    /// question about the held environment.
    pub(super) fn compiles_probe(&self) -> bool {
        matches!(
            self,
            Self::TypeOfExpression { .. } | Self::TypeSearch { .. }
        )
    }

    /// The exact queries submitted for this verb. `currentScopeView` has none.
    ///
    /// Enumeration and `rowFit` currently reuse the admitted inspection batch
    /// (`ScopeBrowse`, `Browse`). That batch compiles its inspection module, so
    /// these verbs are not yet the zero-compile world reads described by the
    /// effect docs. A non-compiling path needs a world-owned `Browse` question
    /// (the public-module question kind currently admits only
    /// `StructuredInfo`/`StructuredType`) and a held-world current-scope
    /// enumeration. `rowFit` additionally reads availability from the browsed
    /// entries because the Lookup excision removes the keyed `ExactName`
    /// query.
    pub(super) fn inspection_queries(&self) -> Vec<InspectionQuery> {
        match self {
            Self::CurrentScopeView => Vec::new(),
            Self::ScopeBindings { .. } => vec![InspectionQuery::ScopeBrowse],
            Self::ModuleExports { module, .. } => vec![InspectionQuery::Browse {
                module: module.clone(),
                expanded: false,
            }],
            Self::RowFit { identifier, .. } => vec![row_fit_query(identifier)],
            Self::TypeOfExpression { expression, .. } => {
                vec![InspectionQuery::TypeOf(expression.clone())]
            }
            Self::TypeSearch { pattern, .. } => vec![InspectionQuery::TypeSearch(pattern.clone())],
        }
    }
}

/// Row fit asks about an identifier's availability under the executing row.
/// Every `InfoEntry` carries `thingAvailability`, which calls
/// `signatureAvailability` for values and reads the solved row for other
/// declarations, so browsing the name's owner yields typed availability.
///
/// Session modules are private to the compiler and refuse module browsing, so
/// a reference that belongs to one is resolved against the executing scope.
/// Every other reference names its defining public module explicitly. The
/// Lookup excision removes `InspectionQuery::ExactName`; until a keyed,
/// non-enumerating availability query lands, this reads the browsed entries.
fn row_fit_query(identifier: &IdentifierRef) -> InspectionQuery {
    if tidepool_repr::SessionModule::is_reserved_name(&identifier.module) {
        InspectionQuery::ScopeBrowse
    } else {
        InspectionQuery::Browse {
            module: identifier.module.clone(),
            expanded: true,
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
#[derive(Clone, Debug, PartialEq, Eq)]
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct NameList {
    entries: Vec<NameEntry>,
    observed: usize,
    limit: usize,
    provenance: ScopeProvenance,
}

impl NameList {
    fn from_entries(entries: Vec<InfoEntry>, provenance: ScopeProvenance) -> Self {
        // `entries` is the decoder's complete list. Measure the observed count
        // before truncation and never recompute it from the retained prefix.
        let observed = entries.len();
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TypeMatches {
    matches: Vec<TypeMatch>,
    observed: usize,
    limit: usize,
    provenance: ScopeProvenance,
}

impl TypeMatches {
    fn from_matches(matches: Vec<RuntimeTypeMatch>, provenance: ScopeProvenance) -> Self {
        // The decoder returns every match; count before applying the limit so
        // the answer reports the complete observation, not the retained prefix.
        let observed = matches.len();
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

#[derive(Clone, Debug, PartialEq, Eq)]
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
        (expected != entry).then(|| CompilerQueryAnswer::ScopeChanged {
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
    if before != current {
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
            CompilerQueryAnswer::Names(NameList::from_entries(entries, before))
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
fn refusal_query(query: &CompilerQuery, before: &ScopeProvenance, name: &str) -> NameQuery {
    let scope = match query {
        CompilerQuery::ModuleExports { module, .. } => NameScope::PublicModule(module.clone()),
        CompilerQuery::RowFit { identifier, .. }
            if !tidepool_repr::SessionModule::is_reserved_name(&identifier.module) =>
        {
            NameScope::PublicModule(identifier.module.clone())
        }
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
        let needs_compiler = query.plans_compiler_work();
        let snapshot = self
            .access
            .with_machine(context.clone(), move |session, context, _| {
                let view = actor_compile_view(session, context, &snapshot_source)?;
                let provenance =
                    current_scope_provenance(&view, &snapshot_source, NameScope::Current);
                // `currentScopeView` reads the view it already holds; it must
                // not capture the compiler inputs a checked query would need.
                let inputs = needs_compiler.then(|| {
                    session
                        .capture_inspection_inputs(view.session_view())
                        .map_err(|error| error.to_string())
                });
                Ok((view, provenance, inputs))
            })
            .await?;
        let (compile_view, provenance, inputs) = snapshot;

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

        if !query.plans_compiler_work() {
            return self
                .access
                .with_continuation_machine(context, move |session, _, _, settlement| {
                    session
                        .resume_classified(
                            hole,
                            CompilerQueryAnswer::ScopeView(provenance),
                            settlement,
                        )
                        .map_err(classify_resumption)
                })
                .await;
        }

        let query = Arc::new(query);
        let queries = query.inspection_queries();
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

    fn typed_identifier(namespace: IdentifierNamespace) -> IdentifierRef {
        IdentifierRef {
            module: "Data.Thing".into(),
            name: "Thing".into(),
            namespace,
        }
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
            CompilerQuery::RowFit {
                expected_scope: expected.clone(),
                identifier: identifier("Data.Thing", "thing"),
            },
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
            vec![InspectionQuery::ScopeBrowse]
        );
        assert_eq!(
            CompilerQuery::ModuleExports {
                expected_scope: None,
                module: "Data.Thing".into()
            }
            .inspection_queries(),
            vec![InspectionQuery::Browse {
                module: "Data.Thing".into(),
                expanded: false
            }]
        );
        assert_eq!(
            CompilerQuery::RowFit {
                expected_scope: None,
                identifier: identifier("Data.Thing", "thing")
            }
            .inspection_queries(),
            vec![InspectionQuery::Browse {
                module: "Data.Thing".into(),
                expanded: true
            }]
        );
        assert_eq!(
            CompilerQuery::RowFit {
                expected_scope: None,
                identifier: identifier("Tidepool.Session.Val.G3", "x")
            }
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
            vec![InspectionQuery::TypeSearch("a -> a".into())]
        );
    }

    #[test]
    fn enumeration_never_plans_a_probe_compile() {
        // The handler never plans `TypeOf`/`TypeSearch` for an enumeration.
        // Runtime-level `inspection.compile_attempts == 0` remains pending:
        // the admitted batch still compiles its inspection module today.
        assert!(!CompilerQuery::CurrentScopeView.plans_compiler_work());
        assert!(CompilerQuery::CurrentScopeView
            .inspection_queries()
            .is_empty());
        for query in compiler_verbs() {
            assert!(query.plans_compiler_work(), "{query:?}");
            if !query.compiles_probe() {
                let planned = query.inspection_queries();
                assert!(
                    planned.iter().all(|planned| matches!(
                        planned,
                        InspectionQuery::ScopeBrowse
                            | InspectionQuery::Browse { .. }
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
                            InspectionQuery::TypeOf(_) | InspectionQuery::TypeSearch(_)
                        )),
                    "probe verb planned no probe query: {query:?}"
                );
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
                        if *seen_before == before && *seen_after == after
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
                        if *seen_before == stale && *seen_after == entry_view
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
                        if *seen_before == stale && *seen_after == entry_view
                ),
                "{query:?} reported the moved view instead of refusing the stale pin at entry"
            );
        }
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
        assert!(matches!(answer, CompilerQueryAnswer::ScopeView(ref seen) if *seen == before));
    }

    #[test]
    fn a_matching_pin_answers_against_the_entry_view() {
        let entry_view = provenance(7, "world-a");
        let requested = identifier("Data.Thing", "thing");
        let answer = compiler_query_answer(
            &CompilerQuery::RowFit {
                expected_scope: Some(entry_view.clone()),
                identifier: requested.clone(),
            },
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
                assert_eq!(fit.provenance, entry_view);
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
                &CompilerQuery::RowFit {
                    expected_scope: None,
                    identifier: requested.clone(),
                },
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
            &CompilerQuery::RowFit {
                expected_scope: None,
                identifier: typed_identifier(IdentifierNamespace::Type),
            },
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
                &CompilerQuery::RowFit {
                    expected_scope: None,
                    identifier: typed_identifier(IdentifierNamespace::Type),
                },
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
                &CompilerQuery::RowFit {
                    expected_scope: None,
                    identifier: requested.clone(),
                },
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
            &CompilerQuery::RowFit {
                expected_scope: None,
                identifier: identifier("Data.Thing", "absent"),
            },
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
            &CompilerQuery::RowFit {
                expected_scope: None,
                identifier: typed_identifier(IdentifierNamespace::Type),
            },
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
            &CompilerQuery::RowFit {
                expected_scope: None,
                identifier: identifier("Data.Thing", "absent"),
            },
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
            &CompilerQuery::RowFit {
                expected_scope: None,
                identifier: identifier("Data.Missing", "thing"),
            },
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
            &CompilerQuery::RowFit {
                expected_scope: None,
                identifier: identifier("Data.Thing", "thing"),
            },
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
                assert_eq!(expression.provenance, before);
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
    fn enumeration_bounds_rows_and_preserves_the_observed_count() {
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
                assert_eq!(names.observed, QUERY_RESULT_LIMIT + 3);
                assert_eq!(names.limit, QUERY_RESULT_LIMIT);
                assert_eq!(names.provenance, entry_view);
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
                assert_eq!(names.provenance, entry_view);
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
                assert_eq!(matches.provenance, entry_view);
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
