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
//! a runtime-owner follow-up. Until a request pins an expected provenance, only
//! movement observed during a query is refused.
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
    NameScope, QueryError, ScopeProvenance, TypeMatch as RuntimeTypeMatch,
    TypeMatchQuality as RuntimeTypeMatchQuality,
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
pub(super) const QUERY_RESULT_LIMIT: usize = 256;

/// A decoded compiler question. The wire enum lives in
/// `generated::introspection`; this type is the actor's own closed set so
/// planning and projection do not depend on generated spelling.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CompilerQuery {
    CurrentScopeView,
    ScopeBindings,
    ModuleExports(String),
    RowFit(IdentifierRef),
    TypeOfExpression(String),
    TypeSearch(String),
}

impl CompilerQuery {
    pub(super) fn operation(&self) -> &'static str {
        match self {
            Self::CurrentScopeView => "currentScopeView",
            Self::ScopeBindings => "scopeBindings",
            Self::ModuleExports(_) => "moduleExports",
            Self::RowFit(_) => "rowFit",
            Self::TypeOfExpression(_) => "typeOfExpression",
            Self::TypeSearch(_) => "typeSearch",
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
        matches!(self, Self::TypeOfExpression(_) | Self::TypeSearch(_))
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
            Self::ScopeBindings => vec![InspectionQuery::ScopeBrowse],
            Self::ModuleExports(module) => vec![InspectionQuery::Browse {
                module: module.clone(),
                expanded: false,
            }],
            Self::RowFit(identifier) => vec![row_fit_query(identifier)],
            Self::TypeOfExpression(expression) => {
                vec![InspectionQuery::TypeOf(expression.clone())]
            }
            Self::TypeSearch(pattern) => vec![InspectionQuery::TypeSearch(pattern.clone())],
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
    if is_session_module(&identifier.module) {
        InspectionQuery::ScopeBrowse
    } else {
        InspectionQuery::Browse {
            module: identifier.module.clone(),
            expanded: true,
        }
    }
}

/// `inspectModule` refuses compiler-generated session modules
/// (`parseSessionModule` in `Tidepool.Introspection`). Asking anyway would be
/// a guaranteed `ModuleNotFound`, so route those references to the scope.
fn is_session_module(module: &str) -> bool {
    module.starts_with("Tidepool.Session.")
}

/// The compile-view identity an answer is relative to.
///
/// Reuse the structured-introspection identity so `currentScopeView`, `info`,
/// and `typeOf` name the same snapshot for one view. This is still a
/// compile-view digest, not the service-issued world identity: the contract's
/// `fingerprint = hex(CompilerWorld::logical_id())` requires a runtime accessor
/// to the published world association that avoids a cold bootstrap. Until that
/// lands this digest detects a moved view and must not be presented as
/// `logical_id`.
fn current_scope_provenance(
    compile_view: &crate::ActorCompileView,
    source: &ActorWorkbenchSource,
    scope: NameScope,
) -> ScopeProvenance {
    super::structured_provenance(compile_view, source, scope)
}

/// Whether a callable's effects fit the inspecting actor's row. Mirrors the
/// protocol `Fit` constructors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Fit {
    Available,
    Polymorphic,
    Unavailable,
    Unknown,
}

impl From<InspectionAvailability> for Fit {
    fn from(availability: InspectionAvailability) -> Self {
        match availability {
            InspectionAvailability::Available => Self::Available,
            InspectionAvailability::Polymorphic => Self::Polymorphic,
            InspectionAvailability::Unavailable => Self::Unavailable,
            InspectionAvailability::Unknown => Self::Unknown,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct NameEntry {
    name: String,
    module: Option<String>,
    kind: String,
    display: String,
    availability: Fit,
    references: Vec<IdentifierRef>,
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
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct NameList {
    entries: Vec<NameEntry>,
    observed: usize,
    limit: usize,
}

impl NameList {
    fn from_entries(entries: Vec<InfoEntry>) -> Self {
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
    availability: Fit,
    references: Vec<IdentifierRef>,
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
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TypeMatches {
    matches: Vec<TypeMatch>,
    observed: usize,
    limit: usize,
}

impl TypeMatches {
    fn from_matches(matches: Vec<RuntimeTypeMatch>) -> Self {
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
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ExpressionType {
    source: String,
    display: String,
    availability: Fit,
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

/// Project one verb's answer. Staleness is decided first: a view that moved
/// while the query ran is a refusal, never an empty or stale success.
pub(super) fn compiler_query_answer(
    query: &CompilerQuery,
    before: ScopeProvenance,
    current: ScopeProvenance,
    outcome: BatchOutcome,
) -> CompilerQueryAnswer {
    if matches!(query, CompilerQuery::CurrentScopeView) {
        return CompilerQueryAnswer::ScopeView(before);
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
        (CompilerQuery::ScopeBindings, InspectionResult::Browse { entries, .. })
        | (CompilerQuery::ModuleExports(_), InspectionResult::Browse { entries, .. }) => {
            CompilerQueryAnswer::Names(NameList::from_entries(entries))
        }
        (CompilerQuery::RowFit(identifier), InspectionResult::Browse { entries, .. }) => {
            CompilerQueryAnswer::Fit(fit_from_entries(identifier, &entries))
        }
        (
            CompilerQuery::TypeOfExpression(_),
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
        (CompilerQuery::TypeOfExpression(_), InspectionResult::Rejected { diagnostic })
        | (CompilerQuery::TypeSearch(_), InspectionResult::Rejected { diagnostic }) => {
            CompilerQueryAnswer::ExpressionRejected(vec![Diagnostic::error(diagnostic)])
        }
        (CompilerQuery::TypeSearch(_), InspectionResult::TypeMatches { matches, .. }) => {
            CompilerQueryAnswer::Matches(TypeMatches::from_matches(matches))
        }
        (
            CompilerQuery::ModuleExports(_) | CompilerQuery::RowFit(_),
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

/// Project availability from the entries that can *be* this identifier. Prefer
/// the requested defining module: a re-export reports the defining module,
/// which legitimately differs from the module the caller named. When a module
/// exports both a type and its constructor under the same spelling, the
/// requested namespace picks between them. `InfoEntry` does not carry a
/// namespace, so its `kind` string is compared against the one this namespace
/// can produce.
///
/// References are not identity: `infoReferences` are the entry's parent and
/// component references, not the entry itself, so a reference hit can name a
/// related declaration. When several candidates remain, only report a fit they
/// all agree on; a convenient first row cannot stand in for a row observation.
/// Absence means the executing environment did not expose the identifier —
/// report `Unknown`, never `Unavailable`.
fn fit_from_entries(identifier: &IdentifierRef, entries: &[InfoEntry]) -> Fit {
    let named = entries
        .iter()
        .filter(|entry| entry.name == identifier.name)
        .collect::<Vec<_>>();
    let namespaced = named
        .iter()
        .copied()
        .filter(|entry| kind_serves_namespace(&entry.kind, identifier.namespace))
        .collect::<Vec<_>>();
    let candidates = if namespaced.is_empty() {
        named
    } else {
        namespaced
    };
    if candidates.is_empty() {
        return Fit::Unknown;
    }
    let defining = candidates
        .iter()
        .copied()
        .filter(|entry| entry.module.as_deref() == Some(identifier.module.as_str()))
        .collect::<Vec<_>>();
    let candidates = if defining.is_empty() {
        candidates
    } else {
        defining
    };
    let mut fits = candidates.iter().map(|entry| Fit::from(entry.availability));
    let first = fits.next().expect("candidates is nonempty");
    if fits.all(|fit| fit == first) {
        first
    } else {
        Fit::Unknown
    }
}

/// `thingKind` spellings that can carry each wire namespace.
fn kind_serves_namespace(kind: &str, namespace: IdentifierNamespace) -> bool {
    match namespace {
        IdentifierNamespace::Value => matches!(kind, "value" | "class-method"),
        IdentifierNamespace::Type => matches!(kind, "type" | "coercion"),
        IdentifierNamespace::Constructor => kind == "constructor",
        IdentifierNamespace::Field => kind == "record-selector",
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
                Self::Fit(fit) => visit_fit(table, visitor, *fit),
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
    fit: Fit,
) -> Result<(), BridgeError> {
    visit_core(
        table,
        visitor,
        match fit {
            Fit::Available => "FitAvailable",
            Fit::Polymorphic => "FitPolymorphic",
            Fit::Unavailable => "FitUnavailable",
            Fit::Unknown => "FitUnknown",
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
        visit_fit(table, visitor, entry.availability)?;
        visit_structural_list(table, visitor, &entry.references, |reference, visitor| {
            visit_introspection_identifier(table, visitor, reference)
        })
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
        actor_haskell_int(names.limit as u64, "name list limit")?.visit(table, visitor)
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
        visit_fit(table, visitor, expression.availability)?;
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
        visit_fit(table, visitor, found.availability)?;
        visit_structural_list(table, visitor, &found.references, |reference, visitor| {
            visit_introspection_identifier(table, visitor, reference)
        })
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
        actor_haskell_int(matches.limit as u64, "type match limit")?.visit(table, visitor)
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

impl<H, O> ResidentActorWorkbench<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    /// Service one compiler-query verb without holding the resident machine
    /// while the compiler runs. The suspended continuation remains the actor's
    /// exclusive turn, mirroring `resume_structured_introspection`.
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
    use tidepool_runtime::session::IdentifierNamespace;

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

    fn entry(name: &str, module: &str, availability: InspectionAvailability) -> InfoEntry {
        InfoEntry {
            name: name.into(),
            module: Some(module.into()),
            kind: "value".into(),
            display: format!("{name} :: Int"),
            availability,
            references: vec![identifier(module, name)],
        }
    }

    fn browse(entries: Vec<InfoEntry>) -> InspectionResult {
        InspectionResult::Browse {
            module: String::new(),
            expanded: true,
            entries,
        }
    }

    fn successful_result(query: &CompilerQuery) -> InspectionResult {
        match query {
            CompilerQuery::CurrentScopeView => unreachable!("no compiler result"),
            CompilerQuery::ScopeBindings
            | CompilerQuery::ModuleExports(_)
            | CompilerQuery::RowFit(_) => {
                browse(vec![entry(
                    "thing",
                    "Data.Thing",
                    InspectionAvailability::Available,
                )])
            }
            CompilerQuery::TypeOfExpression(source) => InspectionResult::Type {
                expression: source.clone(),
                display: "Int".into(),
                availability: InspectionAvailability::Available,
            },
            CompilerQuery::TypeSearch(_) => InspectionResult::TypeMatches {
                query: "Int -> Int".into(),
                matches: vec![RuntimeTypeMatch {
                    name: "succ".into(),
                    module: Some("Prelude".into()),
                    signature: "Int -> Int".into(),
                    quality: RuntimeTypeMatchQuality::Exact,
                    availability: InspectionAvailability::Available,
                    references: vec![identifier("Prelude", "succ")],
                }],
            },
        }
    }

    fn compiler_verbs() -> Vec<CompilerQuery> {
        vec![
            CompilerQuery::ScopeBindings,
            CompilerQuery::ModuleExports("Data.Thing".into()),
            CompilerQuery::RowFit(identifier("Data.Thing", "thing")),
            CompilerQuery::TypeOfExpression("1 + 1".into()),
            CompilerQuery::TypeSearch("Int -> Int".into()),
        ]
    }

    #[test]
    fn translation_maps_each_verb_to_its_owner_query() {
        assert!(CompilerQuery::CurrentScopeView
            .inspection_queries()
            .is_empty());
        assert_eq!(
            CompilerQuery::ScopeBindings.inspection_queries(),
            vec![InspectionQuery::ScopeBrowse]
        );
        assert_eq!(
            CompilerQuery::ModuleExports("Data.Thing".into()).inspection_queries(),
            vec![InspectionQuery::Browse {
                module: "Data.Thing".into(),
                expanded: false
            }]
        );
        assert_eq!(
            CompilerQuery::RowFit(identifier("Data.Thing", "thing")).inspection_queries(),
            vec![InspectionQuery::Browse {
                module: "Data.Thing".into(),
                expanded: true
            }]
        );
        assert_eq!(
            CompilerQuery::RowFit(identifier("Tidepool.Session.Val.G3", "x")).inspection_queries(),
            vec![InspectionQuery::ScopeBrowse]
        );
        assert_eq!(
            CompilerQuery::TypeOfExpression("1 + 1".into()).inspection_queries(),
            vec![InspectionQuery::TypeOf("1 + 1".into())]
        );
        assert_eq!(
            CompilerQuery::TypeSearch("a -> a".into()).inspection_queries(),
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
    fn row_fit_projects_observed_availability() {
        let identifier = identifier("Data.Thing", "thing");
        for (availability, expected) in [
            (InspectionAvailability::Available, Fit::Available),
            (InspectionAvailability::Polymorphic, Fit::Polymorphic),
            (InspectionAvailability::Unavailable, Fit::Unavailable),
            (InspectionAvailability::Unknown, Fit::Unknown),
        ] {
            let answer = compiler_query_answer(
                &CompilerQuery::RowFit(identifier.clone()),
                provenance(1, "world"),
                provenance(1, "world"),
                BatchOutcome::Answer(Ok(browse(vec![entry(
                    "thing",
                    "Data.Thing",
                    availability,
                )]))),
            );
            assert!(
                matches!(answer, CompilerQueryAnswer::Fit(fit) if fit == expected),
                "{availability:?} projected incorrectly"
            );
        }
    }

    #[test]
    fn row_fit_reports_ambiguity_as_unknown_unless_every_candidate_agrees() {
        let identifier = identifier("Data.Thing", "thing");
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
                &CompilerQuery::RowFit(identifier.clone()),
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
            CompilerQueryAnswer::Fit(Fit::Available)
        ));
        assert!(matches!(
            answer(ambiguous(&[
                InspectionAvailability::Available,
                InspectionAvailability::Unavailable,
            ])),
            CompilerQueryAnswer::Fit(Fit::Unknown)
        ));
    }

    #[test]
    fn missing_row_fit_entry_is_unknown_not_unavailable() {
        let answer = compiler_query_answer(
            &CompilerQuery::RowFit(identifier("Data.Thing", "absent")),
            provenance(1, "world"),
            provenance(1, "world"),
            BatchOutcome::Answer(Ok(browse(vec![entry(
                "other",
                "Data.Thing",
                InspectionAvailability::Available,
            )]))),
        );
        assert!(matches!(answer, CompilerQueryAnswer::Fit(Fit::Unknown)));
    }

    #[test]
    fn row_fit_reads_the_named_entry_not_an_entry_that_references_it() {
        let constructor = InfoEntry {
            name: "MkThing".into(),
            module: Some("Data.Thing".into()),
            kind: "constructor".into(),
            display: "MkThing :: Int -> Thing".into(),
            availability: InspectionAvailability::Available,
            references: vec![IdentifierRef {
                module: "Data.Thing".into(),
                name: "Thing".into(),
                namespace: IdentifierNamespace::Type,
            }],
        };
        let ty = InfoEntry {
            name: "Thing".into(),
            module: Some("Data.Thing".into()),
            kind: "type".into(),
            display: "data Thing".into(),
            availability: InspectionAvailability::Unavailable,
            references: Vec::new(),
        };
        let answer = compiler_query_answer(
            &CompilerQuery::RowFit(IdentifierRef {
                module: "Data.Thing".into(),
                name: "Thing".into(),
                namespace: IdentifierNamespace::Type,
            }),
            provenance(1, "world"),
            provenance(1, "world"),
            BatchOutcome::Answer(Ok(browse(vec![constructor, ty]))),
        );
        assert!(matches!(answer, CompilerQueryAnswer::Fit(Fit::Unavailable)));
    }

    #[test]
    fn row_fit_distinguishes_missing_from_unknown_module() {
        let missing = compiler_query_answer(
            &CompilerQuery::RowFit(identifier("Data.Thing", "absent")),
            provenance(1, "world"),
            provenance(1, "world"),
            BatchOutcome::Answer(Ok(browse(Vec::new()))),
        );
        assert!(matches!(missing, CompilerQueryAnswer::Fit(Fit::Unknown)));

        let unknown_module = compiler_query_answer(
            &CompilerQuery::RowFit(identifier("Data.Missing", "thing")),
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
        let reexport = InfoEntry {
            name: "thing".into(),
            module: Some("Internal.Thing".into()),
            kind: "value".into(),
            display: "thing :: Int".into(),
            availability: InspectionAvailability::Unavailable,
            references: Vec::new(),
        };
        let answer = compiler_query_answer(
            &CompilerQuery::RowFit(identifier("Data.Thing", "thing")),
            provenance(1, "world"),
            provenance(1, "world"),
            BatchOutcome::Answer(Ok(browse(vec![reexport]))),
        );
        assert!(matches!(answer, CompilerQueryAnswer::Fit(Fit::Unavailable)));
    }

    #[test]
    fn module_not_found_is_a_typed_refusal() {
        let answer = compiler_query_answer(
            &CompilerQuery::ModuleExports("Data.Missing".into()),
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
    fn a_rejected_probe_is_a_typed_diagnostic() {
        let answer = compiler_query_answer(
            &CompilerQuery::TypeOfExpression("1 +".into()),
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
            &CompilerQuery::TypeOfExpression("1 + 1".into()),
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
                assert_eq!(expression.availability, Fit::Polymorphic);
                assert_eq!(expression.provenance, before);
            }
            other => panic!("expected an expression type, got {}", answer_name(&other)),
        }
    }

    #[test]
    fn probe_infrastructure_failure_is_not_an_expression_rejection() {
        let answer = compiler_query_answer(
            &CompilerQuery::TypeSearch("a -> a".into()),
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
            &CompilerQuery::ScopeBindings,
            provenance(1, "world"),
            provenance(1, "world"),
            BatchOutcome::Answer(Ok(browse(entries))),
        );
        match answer {
            CompilerQueryAnswer::Names(names) => {
                assert_eq!(names.entries.len(), QUERY_RESULT_LIMIT);
                assert_eq!(names.observed, QUERY_RESULT_LIMIT + 3);
                assert_eq!(names.limit, QUERY_RESULT_LIMIT);
            }
            other => panic!("expected a name list, got {}", answer_name(&other)),
        }
    }

    #[test]
    fn module_exports_projects_browse_entries() {
        let answer = compiler_query_answer(
            &CompilerQuery::ModuleExports("Data.Thing".into()),
            provenance(1, "world"),
            provenance(1, "world"),
            BatchOutcome::Answer(Ok(browse(vec![entry(
                "thing",
                "Data.Thing",
                InspectionAvailability::Available,
            )]))),
        );
        match answer {
            CompilerQueryAnswer::Names(names) => {
                assert_eq!(names.entries.len(), 1);
                assert_eq!(names.entries[0].name, "thing");
                assert_eq!(names.observed, 1);
                assert_eq!(names.limit, QUERY_RESULT_LIMIT);
            }
            other => panic!("expected a name list, got {}", answer_name(&other)),
        }
    }

    #[test]
    fn type_search_projects_quality_and_availability() {
        let answer = compiler_query_answer(
            &CompilerQuery::TypeSearch("Int -> Int".into()),
            provenance(1, "world"),
            provenance(1, "world"),
            BatchOutcome::Answer(Ok(successful_result(&CompilerQuery::TypeSearch(
                "Int -> Int".into(),
            )))),
        );
        match answer {
            CompilerQueryAnswer::Matches(matches) => {
                assert_eq!(matches.observed, 1);
                assert_eq!(matches.limit, QUERY_RESULT_LIMIT);
                assert_eq!(matches.matches.len(), 1);
                assert_eq!(matches.matches[0].quality, TypeMatchQuality::Exact);
                assert_eq!(matches.matches[0].availability, Fit::Available);
            }
            other => panic!("expected type matches, got {}", answer_name(&other)),
        }
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
