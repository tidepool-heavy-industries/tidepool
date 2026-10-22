//! Scoped compiler verbs share the admitted held-World query owner.
//!
//! CurrentScopeView returns its entry snapshot. Pinned compiler answers require
//! that snapshot at admission and the same source/view at final resumption.
//! RowFit uses one canonical reference and the actual GHC row solver; search's
//! result cap bounds encoded output, not candidate hydration or solver work.

use tidepool_bridge::FromHaskell;
use tidepool_bridge_derive::FromHaskell as DeriveFromHaskell;
use tidepool_runtime::session::{
    IdentifierRef, InspectionAvailability, NameScope, QueryError, ScopeProvenance,
    TypeMatch as RuntimeTypeMatch, TypeMatchQuality as RuntimeTypeMatchQuality,
    SCOPE_BINDING_LIMIT,
};

use super::structured_introspection::{
    visit_introspection_identifier, visit_introspection_provenance,
    visit_introspection_query_error, visit_structural_list,
};
use super::to_haskell::{actor_haskell_int, visit_core, visit_named};
use super::*;

/// Presentation/transport cap shared with scopeBindings. Search can inspect
/// more candidates than it returns; this does not bound TYPE hydration or GHC
/// solver work. The decoder rejects responses exceeding the requested cap.
pub(super) const QUERY_RESULT_LIMIT: usize = SCOPE_BINDING_LIMIT;

/// A decoded compiler question. The wire enum lives in
/// `generated::introspection`; this type is the actor's own closed set so
/// planning and projection do not depend on generated spelling.
#[derive(Clone, Debug)]
pub(crate) enum CompilerQuery {
    CurrentScopeView,
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

fn same_expected_scope(left: &Option<ScopeProvenance>, right: &Option<ScopeProvenance>) -> bool {
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
                Self::RowFit {
                    expected_scope: left_scope,
                    identifier: left_identifier,
                },
                Self::RowFit {
                    expected_scope: right_scope,
                    identifier: right_identifier,
                },
            ) => {
                same_expected_scope(left_scope, right_scope) && left_identifier == right_identifier
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
            ) => {
                same_expected_scope(left_scope, right_scope) && left_expression == right_expression
            }
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
    /// Preserve the canonical identifier returned by discovery unchanged.
    pub(super) fn row_fit(
        expected_scope: Option<ScopeProvenance>,
        identifier: IdentifierRef,
    ) -> Self {
        Self::RowFit {
            expected_scope,
            identifier,
        }
    }

    pub(super) fn operation(&self) -> &'static str {
        match self {
            Self::CurrentScopeView => "currentScopeView",
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
            Self::RowFit { expected_scope, .. }
            | Self::TypeOfExpression { expected_scope, .. }
            | Self::TypeSearch { expected_scope, .. } => expected_scope.as_ref(),
        }
    }

    pub(super) fn world_question(
        &self,
    ) -> Option<tidepool_toolchain::compiler_world::WorldObservationQuestion> {
        use tidepool_toolchain::compiler_world::WorldObservationQuestion as Q;
        match self {
            Self::CurrentScopeView => None,
            Self::RowFit { identifier, .. } => Some(Q::RowFit(identifier.clone())),
            Self::TypeOfExpression { expression, .. } => Some(Q::Expression(expression.clone())),
            Self::TypeSearch { pattern, .. } => Some(Q::TypeSearch {
                pattern: pattern.clone(),
                limit: QUERY_RESULT_LIMIT as u32,
            }),
        }
    }
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
        // The lower decoder has checked the encoded cap after canonical
        // identity deduplication. A full cap is a lower bound, not a total.
        let observed = matches.len();
        let matches = matches.into_iter().map(TypeMatch::from).collect();
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
    outcome: Result<
        tidepool_toolchain::compiler_world::WorldObservationOutcome,
        ResidentActorWorkbenchError,
    >,
) -> CompilerQueryAnswer {
    use tidepool_toolchain::compiler_world::{WorldObservationOutcome, WorldQueryRejection};
    use tidepool_toolchain::inspection::ScopedReply;
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
        Err(error) => return compiler_query_failure(query, error),
        Ok(WorldObservationOutcome::Rejected(WorldQueryRejection::Unsupported)) => {
            return CompilerQueryAnswer::QueryError(QueryError::Unsupported(
                "held compiler world lacks this scoped observation context".into(),
            ));
        }
        Ok(WorldObservationOutcome::Rejected(reason)) => {
            return CompilerQueryAnswer::CompilerUnavailable(format!(
                "compiler world refused the query: {reason:?}"
            ));
        }
        Ok(WorldObservationOutcome::Answer(result)) => result,
    };
    match (query, result) {
        (
            CompilerQuery::RowFit { identifier, .. },
            ScopedReply::RowFit {
                identifier: found,
                availability,
            },
        ) if identifier == &found => CompilerQueryAnswer::Fit(Fit {
            identifier: found,
            availability: availability.into(),
            provenance: before,
        }),
        (
            CompilerQuery::TypeOfExpression {
                expression: expected,
                ..
            },
            ScopedReply::Expression {
                expression,
                display,
                availability,
            },
        ) if expected == &expression => CompilerQueryAnswer::Expression(ExpressionType {
            source: expression,
            display,
            availability: availability.into(),
            provenance: before,
        }),
        (
            CompilerQuery::TypeSearch {
                pattern: expected, ..
            },
            ScopedReply::TypeSearch { pattern, matches },
        ) if expected == &pattern => {
            CompilerQueryAnswer::Matches(TypeMatches::from_matches(matches, before))
        }
        (_, ScopedReply::Error(error)) => CompilerQueryAnswer::QueryError(error),
        _ => CompilerQueryAnswer::CompilerUnavailable(
            "scoped answer differs from its admitted question".into(),
        ),
    }
}

fn compiler_query_failure(
    query: &CompilerQuery,
    error: ResidentActorWorkbenchError,
) -> CompilerQueryAnswer {
    let compile = match &error {
        ResidentActorWorkbenchError::Compile(error)
        | ResidentActorWorkbenchError::Resident(ResidentError::Run(
            tidepool_runtime::RuntimeError::Compile(error),
        ))
        | ResidentActorWorkbenchError::WorldInspection(
            tidepool_runtime::session::WorldInspectionError::Runtime(
                tidepool_runtime::RuntimeError::Compile(error),
            ),
        ) => Some(error),
        _ => None,
    };
    if matches!(
        query,
        CompilerQuery::TypeOfExpression { .. } | CompilerQuery::TypeSearch { .. }
    ) {
        if let Some(failure) = compile
            .map(classify_compile)
            .filter(|failure| failure.class == FailureClass::UserHaskell)
        {
            return CompilerQueryAnswer::ExpressionRejected(vec![Diagnostic::error(
                failure.message,
            )]);
        }
    }
    CompilerQueryAnswer::CompilerUnavailable(error.to_string())
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
            Self::ScopeView(_) | Self::Fit(_) | Self::Expression(_) | Self::Matches(_)
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
            return u64::try_from(int).map(Self).map_err(|_| {
                tidepool_bridge::type_mismatch("non-negative scope generation", value)
            });
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
fn word64_payload<'a>(value: &'a HaskellValue, table: &DataConTable) -> Option<&'a HaskellValue> {
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

/// Admission refusals never manufacture a compiler answer or runtime seal.
pub(super) enum ScopedWorldInspection {
    ScopeChanged {
        before: ScopeProvenance,
        after: ScopeProvenance,
    },
    Unsupported(String),
    Completed(
        tidepool_runtime::session::CompletedInspectionWorld<
            tidepool_toolchain::compiler_world::WorldObservationOutcome,
        >,
    ),
}

/// A completed observation checked against its actual source and runtime view
/// in the checkout that consumes it. This carries answers, never a new grant.
pub(super) enum FinishedScopedWorldInspection {
    ScopeChanged {
        before: ScopeProvenance,
        after: ScopeProvenance,
    },
    QueryError(QueryError),
    CompilerUnavailable(String),
    Answer {
        before: ScopeProvenance,
        current: ScopeProvenance,
        outcome: tidepool_toolchain::compiler_world::WorldObservationOutcome,
    },
}

pub(super) fn finish_scoped_world_inspection<H, O>(
    session: &mut ResidentSession<H, O>,
    context: &crate::ActorSessionContext,
    source: &ActorWorkbenchSource,
    request_scope: Option<&RequestWorkbenchScope>,
    inspected: ScopedWorldInspection,
) -> Result<FinishedScopedWorldInspection, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    use FinishedScopedWorldInspection as F;
    let completed = match inspected {
        ScopedWorldInspection::ScopeChanged { before, after } => {
            return Ok(F::ScopeChanged { before, after })
        }
        ScopedWorldInspection::Unsupported(detail) => {
            return Ok(F::QueryError(QueryError::Unsupported(detail)))
        }
        ScopedWorldInspection::Completed(completed) => completed,
    };
    let current =
        match source.inspection_provenance(session, context, request_scope, NameScope::Current) {
            Ok(current) => current,
            Err(error) => return Ok(F::QueryError(unobserved_inspection_source(error)?)),
        };
    if !completed.provenance().same_view(&current) {
        return Ok(F::ScopeChanged {
            before: completed.provenance().clone(),
            after: current,
        });
    }
    let captured_source = public_module_query_source(&context.source_layer)
        .map_err(|_| ActorCompileViewError::UnownedSourceLayer)?;
    let evidence = captured_source.generation_evidence().ok_or_else(|| {
        ResidentActorWorkbenchError::ActorProtocol("world query lost source evidence".into())
    })?;
    let original = context
        .source_dependency()
        .source_lease()?
        .original_source()
        .clone();
    if !session.consume_inspection_world_if_current(&completed, &evidence.reference, &original) {
        return Ok(F::CompilerUnavailable(
            "inspection world base changed before consumption".into(),
        ));
    }
    Ok(F::Answer {
        before: completed.provenance().clone(),
        current,
        outcome: completed.answer().clone(),
    })
}

impl<H, O> ResidentActorWorkbench<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    pub(super) async fn query_scoped_world(
        &self,
        context: crate::ActorSessionContext,
        question: tidepool_toolchain::compiler_world::WorldObservationQuestion,
        expected: Option<ScopeProvenance>,
    ) -> Result<ScopedWorldInspection, ResidentActorWorkbenchError> {
        let source = match public_module_query_source(&context.source_layer) {
            Ok(source) => source,
            Err(QueryError::Unsupported(detail)) => {
                return Ok(ScopedWorldInspection::Unsupported(detail));
            }
            Err(error) => {
                return Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                    "source owner selection returned an unexpected refusal: {error:?}"
                )))
            }
        };
        // Refuse an already stale pin before preparing any TYPE metadata.
        // The actual capture below repeats this after an awaited cold prototype
        // preparation, so a publication during preparation cannot slip through.
        if let Some(expected) = expected.as_ref() {
            let expected = expected.clone();
            let view_source = self.access.source.config.clone();
            let request_scope = self.request_scope.clone();
            let refusal = self
                .access
                .with_machine(context.clone(), move |session, context, _| {
                    let current = view_source.inspection_provenance(
                        session,
                        context,
                        request_scope.as_ref(),
                        NameScope::Current,
                    )?;
                    Ok((!expected.same_view(&current)).then_some(
                        ScopedWorldInspection::ScopeChanged {
                            before: expected,
                            after: current,
                        },
                    ))
                })
                .await?;
            if let Some(refusal) = refusal {
                return Ok(refusal);
            }
        }
        if let tidepool_toolchain::compiler_world::WorldObservationQuestion::RowFit(reference) =
            &question
        {
            if reference.module.is_empty()
                || reference.name.is_empty()
                || reference.module.contains('\0')
                || reference.name.contains('\0')
            {
                return Ok(ScopedWorldInspection::Unsupported(
                    "rowFit requires a canonical identifier with a defining module and name".into(),
                ));
            }
        }
        let current_context = PrivateInspectionContext::capture(
            &self.access.source.config,
            &context,
            self.request_scope.as_ref(),
        )?;
        let scaffold = match &self.private_execution {
            Some(execution)
                if !execution
                    .inspection
                    .as_ref()
                    .is_some_and(|inspection| inspection.matches(&current_context)) =>
            {
                return Ok(ScopedWorldInspection::Unsupported(
                    "query context differs from the admitted private invocation".into(),
                ));
            }
            // Preserve the actual activation input TYPE witness issued before
            // private admission. The immutable invocation context was checked above.
            Some(_) => None,
            None => Some(self.capture_observation_scaffold(&context).await?.scaffold),
        };
        let snapshot_source = source.clone();
        let view_source = self.access.source.config.clone();
        let request_scope = self.request_scope.clone();
        let captured = self
            .access
            .with_machine(context.clone(), move |session, context, _| {
                let evidence = snapshot_source.generation_evidence().ok_or_else(|| {
                    ResidentActorWorkbenchError::ActorProtocol(
                        "world query lacks retained source evidence".into(),
                    )
                })?;
                let original = context
                    .source_dependency()
                    .source_lease()?
                    .original_source()
                    .clone();
                let pin = view_source.inspection_scope(
                    session,
                    context,
                    request_scope.as_ref(),
                    NameScope::Current,
                )?;
                if let Some(expected) = expected {
                    let current = pin.provenance();
                    if !expected.same_view(&current) {
                        return Ok(Err(ScopedWorldInspection::ScopeChanged {
                            before: expected,
                            after: current,
                        }));
                    }
                }
                let admitted = session
                    .capture_inspection_world_in(
                        context.placement.lexical_scope,
                        &evidence.reference,
                        &original,
                    )
                    .map_err(|error| {
                        ResidentActorWorkbenchError::Resident(ResidentError::Session(error))
                    })?;
                let admitted = match scaffold {
                    Some(scaffold) => admitted.with_observation_scaffold(scaffold),
                    None => admitted,
                };
                Ok(Ok((admitted, pin)))
            })
            .await?;
        let (admitted, pin) = match captured {
            Ok(captured) => captured,
            Err(refusal) => return Ok(refusal),
        };
        let work = crate::capabilities::CompilerOwnerScope::current().and_then(|scope| {
            scope
                .with_source_dependency(context.source_dependency())
                .admit_work()
        })?;
        let dependency = context.source_dependency().clone();
        let mut cancel_on_drop = CancelCompilerTransactionOnDrop(Some(work.control()));
        let inspected = crate::call_timing::timed_compile(spawn_blocking_in_span(move || {
            let settled = work.run_source_compiler_phase(
                &dependency, tidepool_toolchain::artifacts::CompileWorkload::Foreground,
                |owner, _| tidepool_runtime::session::run_admitted_world_scope_inspection(
                    owner, admitted,
                    || source.capture_compiler_sources().map_err(|error| match error {
                        crate::SourceInspectionError::Unavailable(detail) => tidepool_runtime::session::WorldInspectionError::SourceUnavailable(detail),
                        crate::SourceInspectionError::Unreadable(detail) => tidepool_runtime::session::WorldInspectionError::SourceUnreadable(detail),
                    }), question, pin,
                ),
            ).map_err(ResidentActorWorkbenchError::from)?;
            tidepool_runtime::session::finish_admitted_world_inspection(settled).map_err(|error| match error {
                tidepool_runtime::session::WorldInspectionError::Runtime(error) => ResidentActorWorkbenchError::Resident(ResidentError::Run(error)),
                error => ResidentActorWorkbenchError::WorldInspection(error),
            })
        })).await.map_err(ResidentActorWorkbenchError::Join)?;
        cancel_on_drop.0 = None;
        inspected.map(ScopedWorldInspection::Completed)
    }

    pub(super) async fn resume_scoped_structured_introspection(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        query: tidepool_runtime::session::NameQuery,
        kind: StructuredInspectionKind,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        use tidepool_toolchain::compiler_world::WorldObservationQuestion as Q;
        let question = match kind {
            StructuredInspectionKind::Info => Q::Info(query),
            StructuredInspectionKind::Type => Q::Type(query),
        };
        let inspected = self
            .query_scoped_world(context.clone(), question, None)
            .await;
        let source = self.access.source.config.clone();
        let request_scope = self.request_scope.clone();
        self.access
            .with_compiler_machine(context, move |session, context, _, settlement| {
                let answer = match inspected {
                    Ok(inspected) => match finish_scoped_world_inspection(
                        session,
                        context,
                        &source,
                        request_scope.as_ref(),
                        inspected,
                    )? {
                        FinishedScopedWorldInspection::ScopeChanged { before, after } => {
                            StructuredIntrospectionAnswer::ScopeChanged { before, after }
                        }
                        FinishedScopedWorldInspection::QueryError(error) => {
                            StructuredIntrospectionAnswer::QueryError(error)
                        }
                        FinishedScopedWorldInspection::CompilerUnavailable(detail) => {
                            StructuredIntrospectionAnswer::CompilerUnavailable(detail)
                        }
                        FinishedScopedWorldInspection::Answer { outcome, .. } => {
                            scoped_structured_answer(kind, &outcome)
                        }
                    },
                    Err(
                        error @ ResidentActorWorkbenchError::CompileView(
                            ActorCompileViewError::UnobservedSourceIdentity,
                        ),
                    ) => StructuredIntrospectionAnswer::QueryError(unobserved_inspection_source(
                        error,
                    )?),
                    Err(error) => {
                        StructuredIntrospectionAnswer::CompilerUnavailable(error.to_string())
                    }
                };
                session
                    .resume_classified(hole, answer, settlement)
                    .map_err(classify_resumption)
            })
            .await
    }

    /// CurrentScopeView is the entry snapshot. Every compiler-backed verb
    /// settles and validates its exact source/view in the checkout that resumes it.
    pub(crate) async fn resume_compiler_query(
        &self,
        context: crate::ActorSessionContext,
        hole: ResidentHole,
        query: CompilerQuery,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        let source = self.access.source.config.clone();
        let request_scope = self.request_scope.clone();
        let Some(question) = query.world_question() else {
            return self
                .access
                .with_continuation_machine(context, move |session, context, _, settlement| {
                    let answer = match source.inspection_provenance(
                        session,
                        context,
                        request_scope.as_ref(),
                        NameScope::Current,
                    ) {
                        Ok(provenance) => CompilerQueryAnswer::ScopeView(provenance),
                        Err(error) => {
                            CompilerQueryAnswer::QueryError(unobserved_inspection_source(error)?)
                        }
                    };
                    session
                        .resume_classified(hole, answer, settlement)
                        .map_err(classify_resumption)
                })
                .await;
        };
        let inspected = self
            .query_scoped_world(context.clone(), question, query.expected_scope().cloned())
            .await;
        self.access
            .with_compiler_machine(context, move |session, context, _, settlement| {
                let answer = match inspected {
                    Ok(inspected) => match finish_scoped_world_inspection(
                        session,
                        context,
                        &source,
                        request_scope.as_ref(),
                        inspected,
                    )? {
                        FinishedScopedWorldInspection::ScopeChanged { before, after } => {
                            CompilerQueryAnswer::ScopeChanged { before, after }
                        }
                        FinishedScopedWorldInspection::QueryError(error) => {
                            CompilerQueryAnswer::QueryError(error)
                        }
                        FinishedScopedWorldInspection::CompilerUnavailable(detail) => {
                            CompilerQueryAnswer::CompilerUnavailable(detail)
                        }
                        FinishedScopedWorldInspection::Answer {
                            before,
                            current,
                            outcome,
                        } => compiler_query_answer(&query, before, current, Ok(outcome)),
                    },
                    Err(
                        error @ ResidentActorWorkbenchError::CompileView(
                            ActorCompileViewError::UnobservedSourceIdentity,
                        ),
                    ) => CompilerQueryAnswer::QueryError(unobserved_inspection_source(error)?),
                    Err(error) => compiler_query_failure(&query, error),
                };
                session
                    .resume_classified(hole, answer, settlement)
                    .map_err(classify_resumption)
            })
            .await
    }
}

fn scoped_structured_answer(
    kind: StructuredInspectionKind,
    answer: &tidepool_toolchain::compiler_world::WorldObservationOutcome,
) -> StructuredIntrospectionAnswer {
    use tidepool_toolchain::compiler_world::{WorldObservationOutcome as O, WorldQueryRejection};
    use tidepool_toolchain::inspection::ScopedReply as R;
    match (kind, answer) {
        (StructuredInspectionKind::Info, O::Answer(R::Info(info))) => {
            StructuredIntrospectionAnswer::Info(info.clone())
        }
        (StructuredInspectionKind::Type, O::Answer(R::Type(info))) => {
            StructuredIntrospectionAnswer::Type(info.clone())
        }
        (_, O::Answer(R::Error(error))) => StructuredIntrospectionAnswer::QueryError(error.clone()),
        (_, O::Rejected(WorldQueryRejection::Unsupported)) => {
            StructuredIntrospectionAnswer::QueryError(QueryError::Unsupported(
                "held compiler world lacks this scoped observation context".into(),
            ))
        }
        (_, O::Rejected(reason)) => StructuredIntrospectionAnswer::CompilerUnavailable(format!(
            "compiler world refused the query: {reason:?}"
        )),
        _ => StructuredIntrospectionAnswer::CompilerUnavailable(
            "scoped answer differs from its admitted structured question".into(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_toolchain::compiler_world::{
        WorldObservationOutcome as O, WorldObservationQuestion as Q, WorldQueryRejection,
    };
    use tidepool_toolchain::inspection::ScopedReply;

    fn provenance(generation: u64, fingerprint: &str) -> ScopeProvenance {
        ScopeProvenance {
            scope: NameScope::Current,
            generation,
            fingerprint: fingerprint.into(),
        }
    }
    fn identifier(name: &str) -> IdentifierRef {
        IdentifierRef {
            module: "Original.Module".into(),
            name: name.into(),
            namespace: tidepool_runtime::session::IdentifierNamespace::Value,
        }
    }

    // Representation/projection controls only; hosted acceptance owns actual
    // compiler answers, effects, source acquisition and runtime authority.
    #[test]
    fn closed_questions_preserve_discovered_identity_and_output_cap() {
        let reference = identifier("discovered");
        assert!(
            matches!(CompilerQuery::row_fit(None, reference.clone()).world_question(), Some(Q::RowFit(found)) if found == reference)
        );
        assert!(
            matches!(CompilerQuery::TypeSearch { expected_scope: None, pattern: "ProbeToken -> ProbeToken".into() }.world_question(), Some(Q::TypeSearch { pattern, limit }) if pattern == "ProbeToken -> ProbeToken" && limit as usize == QUERY_RESULT_LIMIT)
        );
        assert!(CompilerQuery::CurrentScopeView.world_question().is_none());
    }

    #[test]
    fn stale_entry_and_settlement_pins_win_over_answers_and_failures() {
        let old = provenance(1, "old");
        let entry = provenance(2, "entry");
        let after = provenance(2, "different-imports");
        let query = CompilerQuery::row_fit(Some(old.clone()), identifier("x"));
        assert!(
            matches!(entry_scope_refusal(&query, &entry), Some(CompilerQueryAnswer::ScopeChanged { before, after }) if before.same_view(&old) && after.same_view(&entry))
        );
        assert!(
            matches!(compiler_query_answer(&query, entry.clone(), after.clone(), Ok(O::Rejected(WorldQueryRejection::Unavailable))), CompilerQueryAnswer::ScopeChanged { before, after } if before.same_view(&old) && after.same_view(&entry))
        );
        let query = CompilerQuery::row_fit(Some(entry.clone()), identifier("x"));
        assert!(
            matches!(compiler_query_answer(&query, entry.clone(), after.clone(), Err(ResidentActorWorkbenchError::ActorProtocol("unavailable".into()))), CompilerQueryAnswer::ScopeChanged { before, after: seen } if before.same_view(&entry) && seen.same_view(&after))
        );
    }

    #[test]
    fn keyed_fit_preserves_availability_and_refuses_crossed_identity() {
        let pin = provenance(2, "view");
        let query = CompilerQuery::row_fit(None, identifier("x"));
        for availability in [
            InspectionAvailability::Available,
            InspectionAvailability::Unavailable,
            InspectionAvailability::Polymorphic,
            InspectionAvailability::Unknown,
        ] {
            let answer = compiler_query_answer(
                &query,
                pin.clone(),
                pin.clone(),
                Ok(O::Answer(ScopedReply::RowFit {
                    identifier: identifier("x"),
                    availability,
                })),
            );
            assert!(
                matches!(answer, CompilerQueryAnswer::Fit(fit) if fit.identifier == identifier("x") && fit.availability == FitAvailability::from(availability) && fit.provenance.same_view(&pin))
            );
        }
        assert!(matches!(
            compiler_query_answer(
                &query,
                pin.clone(),
                pin,
                Ok(O::Answer(ScopedReply::RowFit {
                    identifier: identifier("y"),
                    availability: InspectionAvailability::Available
                }))
            ),
            CompilerQueryAnswer::CompilerUnavailable(_)
        ));
    }

    #[test]
    fn absent_scaffold_and_source_failure_are_distinct() {
        let pin = provenance(2, "view");
        let query = CompilerQuery::row_fit(None, identifier("x"));
        assert!(matches!(
            compiler_query_answer(
                &query,
                pin.clone(),
                pin.clone(),
                Ok(O::Rejected(WorldQueryRejection::Unsupported))
            ),
            CompilerQueryAnswer::QueryError(QueryError::Unsupported(_))
        ));
        for refusal in [
            WorldQueryRejection::Unavailable,
            WorldQueryRejection::Corrupt,
        ] {
            assert!(matches!(
                compiler_query_answer(&query, pin.clone(), pin.clone(), Ok(O::Rejected(refusal))),
                CompilerQueryAnswer::CompilerUnavailable(_)
            ));
        }
        assert!(matches!(
            compiler_query_failure(
                &query,
                ResidentActorWorkbenchError::WorldInspection(
                    tidepool_runtime::session::WorldInspectionError::SourceUnreadable(
                        "denied".into()
                    )
                )
            ),
            CompilerQueryAnswer::CompilerUnavailable(_)
        ));
    }

    #[test]
    fn expression_answer_preserves_source_type_and_solver_result() {
        let pin = provenance(2, "view");
        let query = CompilerQuery::TypeOfExpression {
            expected_scope: None,
            expression: "queryEcho".into(),
        };
        let answer = compiler_query_answer(
            &query,
            pin.clone(),
            pin.clone(),
            Ok(O::Answer(ScopedReply::Expression {
                expression: "queryEcho".into(),
                display: "ProbeToken -> ProbeToken".into(),
                availability: InspectionAvailability::Polymorphic,
            })),
        );
        assert!(
            matches!(answer, CompilerQueryAnswer::Expression(found) if found.source == "queryEcho" && found.display == "ProbeToken -> ProbeToken" && found.availability == FitAvailability::Polymorphic && found.provenance.same_view(&pin))
        );
        assert!(matches!(
            compiler_query_answer(
                &query,
                pin.clone(),
                pin,
                Ok(O::Answer(ScopedReply::Expression {
                    expression: "other".into(),
                    display: "Int".into(),
                    availability: InspectionAvailability::Available
                }))
            ),
            CompilerQueryAnswer::CompilerUnavailable(_)
        ));
    }
}
