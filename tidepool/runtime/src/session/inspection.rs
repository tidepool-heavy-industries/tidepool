//! GHC-authoritative inspection of the exact source environment used by a
//! resident workbench turn.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use ciborium::value::Value as CborValue;
use serde::Serialize;
use tempfile::TempDir;
use tidepool_extract_cmd::{ExtractCmd, SpawnError};
pub use tidepool_extract_cmd::{
    HostedLookupBatch, HostedLookupDiscovery, HostedLookupOperation, HostedLookupQuery,
};

use crate::{timing, CompileError};

use super::assemble_inspection_module;
pub use tidepool_extract_cmd::LookupNameMode;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InspectionQuery {
    TypeOf(String),
    Info(String),
    LookupName {
        query: String,
        qualifier_module: Option<String>,
        mode: LookupNameMode,
    },
    ExactName(ExactNameQuery),
    TypeSearch(String),
    ScopeBrowse,
    Browse {
        module: String,
        expanded: bool,
    },
    StructuredInfo {
        query: NameQuery,
        provenance: ScopeProvenance,
    },
    StructuredType {
        query: NameQuery,
        provenance: ScopeProvenance,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NameScope {
    Current,
    PublicModule(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameNamespace {
    Any,
    Value,
    Type,
    Constructor,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NameQuery {
    pub scope: NameScope,
    pub namespace: NameNamespace,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExactNameQuery {
    pub scope: NameScope,
    pub namespace: IdentifierNamespace,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeProvenance {
    pub scope: NameScope,
    pub generation: u64,
    pub fingerprint: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentifierNamespace {
    Value,
    Type,
    Constructor,
    Field,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdentifierRef {
    pub module: String,
    pub name: String,
    pub namespace: IdentifierNamespace,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeExpression {
    pub canonical: String,
    pub variables: Vec<String>,
    pub constraints: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeInfo {
    pub identifier: IdentifierRef,
    pub expression: TypeExpression,
    pub provenance: ScopeProvenance,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldInfo {
    pub name: String,
    pub ty: TypeExpression,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConstructorInfo {
    pub identifier: IdentifierRef,
    pub ty: TypeExpression,
    pub arguments: Vec<TypeExpression>,
    pub fields: Vec<FieldInfo>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClassMethodInfo {
    pub identifier: IdentifierRef,
    pub ty: TypeExpression,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeclarationInfo {
    Value(TypeExpression),
    Data {
        parameters: Vec<String>,
        constructors: Vec<ConstructorInfo>,
    },
    Newtype {
        parameters: Vec<String>,
        constructor: ConstructorInfo,
    },
    TypeSynonym {
        parameters: Vec<String>,
        body: TypeExpression,
    },
    Class {
        parameters: Vec<String>,
        superclasses: Vec<TypeExpression>,
        methods: Vec<ClassMethodInfo>,
    },
    Constructor {
        parent: IdentifierRef,
        constructor: ConstructorInfo,
    },
    RecordSelector {
        parent: IdentifierRef,
        ty: TypeExpression,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdentifierInfo {
    pub identifier: IdentifierRef,
    pub declaration: DeclarationInfo,
    pub parent: Option<IdentifierRef>,
    pub provenance: ScopeProvenance,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QueryError {
    Unknown(NameQuery),
    Ambiguous(NameQuery, Vec<IdentifierRef>),
    UnknownModule(String),
    Unsupported(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InfoEntry {
    pub name: String,
    pub module: Option<String>,
    pub kind: String,
    pub display: String,
    pub availability: InspectionAvailability,
    pub references: Vec<IdentifierRef>,
}

/// Whether a callable's required effects fit the inspecting actor's row.
/// Runtime resource grants remain a separate authority check.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InspectionAvailability {
    Available,
    /// The row fits and every closed constraint solves; a constraint stays open
    /// only because the call site decides it. Usable, not uncertain.
    Polymorphic,
    Unavailable,
    Unknown,
}

impl InspectionAvailability {
    fn decode(value: &CborValue, what: &str) -> Result<Self, CompileError> {
        match text(value, what)? {
            "Available" => Ok(Self::Available),
            "Polymorphic" => Ok(Self::Polymorphic),
            "Unavailable" => Ok(Self::Unavailable),
            "Unknown" => Ok(Self::Unknown),
            other => Err(invalid(format!("unknown {what} {other:?}"))),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypeMatchQuality {
    Exact,
    Usable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeMatch {
    pub name: String,
    pub module: Option<String>,
    pub signature: String,
    pub quality: TypeMatchQuality,
    pub availability: InspectionAvailability,
    pub references: Vec<IdentifierRef>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LookupAnswer {
    Info(Vec<InfoEntry>),
    Ambiguous(Vec<InfoEntry>),
    Module {
        module: String,
        entries: Vec<InfoEntry>,
    },
    Missing {
        export_names: Vec<String>,
    },
    ModuleNotFound(String),
}

/// A compiler-issued hosted prefix and the complete size observed before admission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectedMatches<T> {
    pub entries: Vec<T>,
    pub observed: usize,
    pub limit: usize,
}

impl<T> SelectedMatches<T> {
    pub fn truncated(&self) -> bool {
        self.observed > self.entries.len()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SelectedLookupAnswer {
    Info(SelectedMatches<InfoEntry>),
    Ambiguous(SelectedMatches<InfoEntry>),
    Module {
        module: String,
        matches: SelectedMatches<InfoEntry>,
    },
    Missing {
        export_names: Vec<String>,
    },
    ModuleNotFound(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostedOperationIdentity {
    Primary(u32),
    Reference(Vec<u32>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostedLookupAnswer {
    Name {
        ordinal: u32,
        scopes: Vec<NameScope>,
        answer: SelectedLookupAnswer,
    },
    Type {
        ordinal: u32,
        matches: SelectedMatches<TypeMatch>,
    },
    Reference {
        destinations: Vec<u32>,
        scopes: Vec<NameScope>,
        answer: SelectedLookupAnswer,
    },
    Rejected {
        operation: HostedOperationIdentity,
        diagnostic: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostedFallback {
    NotNeeded,
    ZeroBudget,
    Unavailable(String),
    Attempted(Vec<NameScope>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostedDiscoveryCandidate {
    pub query: String,
    pub origins: Vec<String>,
    pub summary: String,
    pub local: bool,
    pub reference: Option<IdentifierRef>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostedDiscoverySelection {
    pub candidates: Vec<HostedDiscoveryCandidate>,
    pub fallback: HostedFallback,
    pub eligible_modules: usize,
    pub submitted_modules: usize,
    pub enumerated_occurrences: usize,
    pub discarded_occurrences: usize,
    pub candidate_limit: usize,
    pub issue: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostedLookupOutput {
    pub answers: Vec<HostedLookupAnswer>,
    pub discovery: Option<HostedDiscoverySelection>,
}

impl HostedLookupOutput {
    /// Check the issuing operation coverage and discovery receipt without reordering results.
    pub fn validate_for(&self, batch: &HostedLookupBatch) -> Result<(), CompileError> {
        batch
            .validate()
            .map_err(|error| invalid(error.to_string()))?;
        validate_hosted_output(batch, self)
    }
}

enum InspectionOutput {
    Complete(Vec<InspectionResult>),
    Hosted(HostedLookupOutput),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InspectionResult {
    Lookup {
        query: String,
        scopes: Vec<NameScope>,
        answer: LookupAnswer,
    },
    Type {
        expression: String,
        display: String,
        availability: InspectionAvailability,
    },
    Info {
        query: String,
        entries: Vec<InfoEntry>,
    },
    Ambiguous {
        query: String,
        entries: Vec<InfoEntry>,
    },
    NotFound {
        query: String,
    },
    ModuleNotFound {
        module: String,
        scopes: Vec<NameScope>,
    },
    Rejected {
        diagnostic: String,
    },
    Browse {
        module: String,
        expanded: bool,
        entries: Vec<InfoEntry>,
    },
    TypeMatches {
        query: String,
        matches: Vec<TypeMatch>,
    },
    StructuredInfo(Result<Box<IdentifierInfo>, QueryError>),
    StructuredType(Result<TypeInfo, QueryError>),
}

impl InspectionResult {
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            Self::Lookup { query, answer, .. } => match answer {
                LookupAnswer::Info(entries)
                | LookupAnswer::Ambiguous(entries)
                | LookupAnswer::Module { entries, .. } => entries
                    .iter()
                    .map(|entry| entry.display.trim())
                    .collect::<Vec<_>>()
                    .join("\n\n"),
                LookupAnswer::Missing { .. } => format!("unknown name `{query}`"),
                LookupAnswer::ModuleNotFound(module) => format!("unknown module `{module}`"),
            },
            Self::Type {
                expression,
                display,
                ..
            } => format!("{expression} :: {display}"),
            Self::Info { entries, .. } => entries
                .iter()
                .map(|entry| entry.display.trim())
                .collect::<Vec<_>>()
                .join("\n\n"),
            Self::Ambiguous { query, entries } => {
                let candidates = entries
                    .iter()
                    .map(|entry| match &entry.module {
                        Some(module) => format!("{module}.{}", entry.name),
                        None => entry.name.clone(),
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("ambiguous name `{query}`; candidates: {candidates}")
            }
            Self::NotFound { query } => format!("unknown name `{query}`"),
            Self::ModuleNotFound { module, .. } => format!("unknown module `{module}`"),
            Self::Rejected { diagnostic } => diagnostic.clone(),
            Self::Browse {
                module, entries, ..
            } => {
                let declarations = entries
                    .iter()
                    .map(|entry| entry.display.trim())
                    .collect::<Vec<_>>()
                    .join("\n");
                if declarations.is_empty() {
                    format!("-- {module}")
                } else {
                    format!("-- {module}\n{declarations}")
                }
            }
            Self::StructuredInfo(Ok(info)) => format!(
                "{} -- {}.{}",
                render_declaration(&info.declaration),
                info.identifier.module,
                info.identifier.name
            ),
            Self::StructuredType(Ok(info)) => format!(
                "{}.{} :: {}",
                info.identifier.module, info.identifier.name, info.expression.canonical
            ),
            Self::StructuredInfo(Err(error)) | Self::StructuredType(Err(error)) => {
                render_query_error(error)
            }
            Self::TypeMatches { matches, .. } => matches
                .iter()
                .map(|entry| format!("{} :: {}", entry.name, entry.signature))
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

fn render_declaration(declaration: &DeclarationInfo) -> String {
    match declaration {
        DeclarationInfo::Value(ty) => ty.canonical.clone(),
        DeclarationInfo::Data { constructors, .. } => {
            format!("data ({} constructors)", constructors.len())
        }
        DeclarationInfo::Newtype { constructor, .. } => {
            format!("newtype ({})", constructor.identifier.name)
        }
        DeclarationInfo::TypeSynonym { body, .. } => format!("type = {}", body.canonical),
        DeclarationInfo::Class { methods, .. } => format!("class ({} methods)", methods.len()),
        DeclarationInfo::Constructor { constructor, .. } => constructor.ty.canonical.clone(),
        DeclarationInfo::RecordSelector { ty, .. } => ty.canonical.clone(),
    }
}

fn render_query_error(error: &QueryError) -> String {
    match error {
        QueryError::Unknown(query) => format!("unknown name `{}`", query.name),
        QueryError::Ambiguous(query, candidates) => format!(
            "ambiguous name `{}`; candidates: {}",
            query.name,
            candidates
                .iter()
                .map(|candidate| format!("{}.{}", candidate.module, candidate.name))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        QueryError::UnknownModule(module) => format!("unknown module `{module}`"),
        QueryError::Unsupported(detail) => detail.clone(),
    }
}

/// Checked inspection authority captured by the resident scope owner.
/// The input inventory cannot be assembled or changed by a caller.
#[derive(Clone, Debug)]
pub struct AdmittedInspectionInputs {
    catalog: tidepool_toolchain::toolchain::CatalogSelection,
    view: super::SessionCompileView,
    values: Vec<super::admission::AdmittedValueInterface>,
}

impl AdmittedInspectionInputs {
    pub(super) fn capture(
        view: super::SessionCompileView,
        values: Vec<super::admission::AdmittedValueInterface>,
        catalog: tidepool_toolchain::toolchain::CatalogSelection,
    ) -> Self {
        Self {
            view,
            values,
            catalog,
        }
    }

    pub fn view(&self) -> &super::SessionCompileView {
        &self.view
    }

    fn matches_request(&self, request: &InspectionRequest<'_>) -> bool {
        request.session_root == self.view.session_root()
            && request.exact_context == self.view.exact_compile_context()
            && request.inject_modules == self.view.injected_module_names()
    }
}

pub struct InspectionRequest<'a> {
    /// Protected declarations and request-local annotations shared by every query in this batch.
    pub exact_context:
        Option<std::sync::Arc<tidepool_toolchain::declaration_join::ExactCompileContext>>,
    pub preamble: &'a str,
    pub imports: &'a str,
    pub include: &'a [&'a Path],
    pub session_root: &'a Path,
    pub inject_modules: &'a [String],
    pub queries: &'a [InspectionQuery],
    /// The actor's GHC effects type alias. Standalone inspections leave it absent.
    pub effects: Option<&'a super::HaskellTypeSource>,
}

/// Hosted lookup shares the ordinary admitted input owner but carries one
/// closed batch instead of an independent query/ordinal registry.
pub struct HostedLookupRequest<'a> {
    pub exact_context:
        Option<std::sync::Arc<tidepool_toolchain::declaration_join::ExactCompileContext>>,
    pub preamble: &'a str,
    pub imports: &'a str,
    pub include: &'a [&'a Path],
    pub session_root: &'a Path,
    pub inject_modules: &'a [String],
    pub batch: &'a HostedLookupBatch,
    pub effects: Option<&'a super::HaskellTypeSource>,
}

fn hosted_queries(batch: &HostedLookupBatch) -> Result<Vec<InspectionQuery>, CompileError> {
    batch
        .operations
        .iter()
        .map(|operation| {
            Ok(match operation {
                HostedLookupOperation::Primary {
                    query: HostedLookupQuery::Name(query),
                    ..
                } => InspectionQuery::LookupName {
                    query: query.query.clone(),
                    qualifier_module: query.qualifier_module.clone(),
                    mode: query.mode,
                },
                HostedLookupOperation::Primary {
                    query: HostedLookupQuery::TypeSearch(query),
                    ..
                } => InspectionQuery::TypeSearch(query.clone()),
                HostedLookupOperation::ExactReference { query, .. } => {
                    InspectionQuery::ExactName(ExactNameQuery {
                        scope: match &query.scope {
                            tidepool_extract_cmd::InspectionScope::Current => NameScope::Current,
                            tidepool_extract_cmd::InspectionScope::PublicModule(owner) => {
                                NameScope::PublicModule(owner.clone())
                            }
                        },
                        namespace: match query.namespace {
                            tidepool_extract_cmd::InspectionNamespace::Value => {
                                IdentifierNamespace::Value
                            }
                            tidepool_extract_cmd::InspectionNamespace::Type => {
                                IdentifierNamespace::Type
                            }
                            tidepool_extract_cmd::InspectionNamespace::Constructor => {
                                IdentifierNamespace::Constructor
                            }
                            tidepool_extract_cmd::InspectionNamespace::Field => {
                                IdentifierNamespace::Field
                            }
                            tidepool_extract_cmd::InspectionNamespace::Any => {
                                return Err(invalid(
                                    "exact reference requires an identifier namespace",
                                ))
                            }
                        },
                        name: query.name.clone(),
                    })
                }
            })
        })
        .collect()
}

pub fn run_hosted_lookup(
    request: HostedLookupRequest<'_>,
    settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
) -> Result<HostedLookupOutput, CompileError> {
    run_hosted_lookup_with_inputs(request, None, settlement)
}

pub fn run_admitted_hosted_lookup(
    request: HostedLookupRequest<'_>,
    view: &super::SessionCompileView,
    inputs: &AdmittedInspectionInputs,
    settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
) -> Result<HostedLookupOutput, CompileError> {
    validate_inspection_view(&request.exact_context, view, inputs)?;
    run_hosted_lookup_with_inputs(request, Some(inputs), settlement)
}

fn run_hosted_lookup_with_inputs(
    request: HostedLookupRequest<'_>,
    inputs: Option<&AdmittedInspectionInputs>,
    settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
) -> Result<HostedLookupOutput, CompileError> {
    request
        .batch
        .validate()
        .map_err(|error| invalid(error.to_string()))?;
    let queries = hosted_queries(request.batch)?;
    let common = InspectionRequest {
        exact_context: request.exact_context,
        preamble: request.preamble,
        imports: request.imports,
        include: request.include,
        session_root: request.session_root,
        inject_modules: request.inject_modules,
        queries: &queries,
        effects: request.effects,
    };
    match run_inspection_batch(common, false, inputs, Some(request.batch), settlement)? {
        InspectionOutput::Hosted(output) => Ok(output),
        InspectionOutput::Complete(_) => Err(invalid(
            "complete inspection output cannot satisfy hosted lookup",
        )),
    }
}

/// Inspect an ordered batch without evaluating it or mutating the resident
/// session. Every query sees the same preamble, imports, session modules, and
/// injected value interfaces as the next ordinary turn. The worker serves the
/// batch through one request while isolating GHC rejection per query. A
/// homogeneous batch of type probes is compiled together first; its original
/// singleton sources remain available if that combined source is rejected.
pub fn run_inspections(
    request: InspectionRequest<'_>,
    settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
) -> Result<Vec<InspectionResult>, CompileError> {
    run_inspections_with_policy(request, false, None, settlement)
}

/// Consume the immutable checked value snapshot captured with the caller's view.
/// This grants only inspection inputs; successful answers still need exact receipts.
pub fn run_admitted_inspections(
    request: InspectionRequest<'_>,
    view: &super::SessionCompileView,
    inputs: &AdmittedInspectionInputs,
    settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
) -> Result<Vec<InspectionResult>, CompileError> {
    validate_inspection_view(&request.exact_context, view, inputs)?;
    run_inspections_with_policy(request, false, Some(inputs), settlement)
}

fn validate_inspection_view(
    exact_context: &Option<
        std::sync::Arc<tidepool_toolchain::declaration_join::ExactCompileContext>,
    >,
    view: &super::SessionCompileView,
    inputs: &AdmittedInspectionInputs,
) -> Result<(), CompileError> {
    if view.session() != inputs.view.session()
        || view.lexical_scope() != inputs.view.lexical_scope()
        || !view.is_current_for(&inputs.view)
        || *exact_context != view.exact_compile_context()
    {
        return Err(invalid("inspection inputs belong to another compile view"));
    }
    Ok(())
}

/// Declaration staging variant: source rejection must remain a request-level
/// structured diagnostic so declaration span remapping is preserved.
pub(super) fn run_inspections_strict(
    request: InspectionRequest<'_>,
    settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
) -> Result<Vec<InspectionResult>, CompileError> {
    run_inspections_with_policy(request, true, None, settlement)
}

fn run_inspections_with_policy(
    request: InspectionRequest<'_>,
    strict: bool,
    inputs: Option<&AdmittedInspectionInputs>,
    settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
) -> Result<Vec<InspectionResult>, CompileError> {
    match run_inspection_batch(request, strict, inputs, None, settlement)? {
        InspectionOutput::Complete(results) => Ok(results),
        InspectionOutput::Hosted(_) => {
            Err(invalid("hosted output cannot satisfy complete inspection"))
        }
    }
}

fn run_inspection_batch(
    request: InspectionRequest<'_>,
    strict: bool,
    inputs: Option<&AdmittedInspectionInputs>,
    hosted: Option<&HostedLookupBatch>,
    settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
) -> Result<InspectionOutput, CompileError> {
    if inputs.is_some_and(|inputs| !inputs.matches_request(&request)) {
        return Err(invalid(
            "inspection request differs from its admitted scope snapshot",
        ));
    }
    let values = inputs.map(|inputs| inputs.values.as_slice());
    if request.queries.is_empty() {
        return if hosted.is_some() {
            Err(invalid("hosted lookup has no physical operations"))
        } else {
            Ok(InspectionOutput::Complete(Vec::new()))
        };
    }
    let temp = TempDir::new()?;
    let output_path = temp.path().join("inspection.cbor");

    let mut command = ExtractCmd::new().map_err(|error| CompileError::Io(error.into()))?;
    command
        .output_dir(temp.path())
        .inspect_out(&output_path)
        .includes(request.include)
        .session_root(request.session_root);
    if values.is_none() {
        command.inject_vals(request.inject_modules);
    }
    if strict {
        command.inspection_strict();
    }
    let issued_imports = request.effects.map(|effects| {
        effects
            .source_imports(&super::SourceImports::from_specs([request.imports]))
            .template_text()
    });
    let imports = match request.effects {
        Some(_) => format!(
            "{}\nqualified Data.Proxy\nqualified Tidepool.Effects.Core\nqualified Tidepool.Agent.Contract\n",
            issued_imports
                .as_deref()
                .expect("effects imports assembled")
        ),
        None => request.imports.to_owned(),
    };
    let type_batch = request
        .queries
        .iter()
        .map(|query| match query {
            InspectionQuery::TypeOf(expression) => Some(expression.clone()),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()
        .filter(|expressions| {
            expressions.len() > 1
                && !request.preamble.contains("__tidepool_inspect_")
                && !request.imports.contains("__tidepool_inspect_")
                && !expressions
                    .iter()
                    .any(|expression| expression.contains("__tidepool_inspect_"))
        });
    let mut shared_environment_source = None;
    let mut sources = BTreeMap::new();
    let mut query_sources = Vec::with_capacity(request.queries.len());
    for (index, query) in request.queries.iter().enumerate() {
        let query_dir = temp.path().join(format!("query-{index}"));
        std::fs::create_dir(&query_dir)?;
        let shares_environment = matches!(
            query,
            InspectionQuery::Info(_)
                | InspectionQuery::LookupName { .. }
                | InspectionQuery::ExactName(_)
                | InspectionQuery::ScopeBrowse
                | InspectionQuery::Browse { .. }
                | InspectionQuery::StructuredInfo { .. }
                | InspectionQuery::StructuredType { .. }
        );
        let source_path = if shares_environment {
            shared_environment_source
                .get_or_insert_with(|| query_dir.join("Expr.hs"))
                .clone()
        } else {
            query_dir.join("Expr.hs")
        };
        let expressions = match query {
            InspectionQuery::TypeOf(expression) => std::slice::from_ref(expression),
            InspectionQuery::Info(_)
            | InspectionQuery::LookupName { .. }
            | InspectionQuery::ExactName(_)
            | InspectionQuery::TypeSearch(_)
            | InspectionQuery::ScopeBrowse
            | InspectionQuery::Browse { .. }
            | InspectionQuery::StructuredInfo { .. }
            | InspectionQuery::StructuredType { .. } => &[],
        };
        let mut source = assemble_inspection_module(request.preamble, &imports, expressions);
        if let Some(effects) = request.effects {
            source.push_str("\n__tidepool_lookup_row :: Data.Proxy.Proxy (");
            source.push_str(effects.expression());
            source.push_str(")\n__tidepool_lookup_row = Data.Proxy.Proxy\n");
        }
        if let InspectionQuery::TypeSearch(query) = query {
            source.push_str("\n__tidepool_lookup_query :: ");
            source.push_str(query);
            source.push_str("\n__tidepool_lookup_query = __tidepool_lookup_query\n");
        }
        std::fs::write(&source_path, &source)?;
        sources.insert(source_path.clone(), source);
        query_sources.push(source_path.clone());
        command.input(&source_path);
        if hosted.is_none() {
            match query {
                InspectionQuery::TypeOf(expression) => {
                    command.inspect_type(expression);
                }
                InspectionQuery::LookupName {
                    query,
                    qualifier_module,
                    mode,
                } => {
                    command.inspect_lookup_name(tidepool_extract_cmd::LookupNameInspection {
                        query: query.clone(),
                        qualifier_module: qualifier_module.clone(),
                        mode: *mode,
                    });
                }
                InspectionQuery::ExactName(query) => {
                    command.inspect_exact_name(tidepool_extract_cmd::ExactNameInspection {
                        scope: request_scope(&query.scope),
                        namespace: match query.namespace {
                            IdentifierNamespace::Value => {
                                tidepool_extract_cmd::InspectionNamespace::Value
                            }
                            IdentifierNamespace::Type => {
                                tidepool_extract_cmd::InspectionNamespace::Type
                            }
                            IdentifierNamespace::Constructor => {
                                tidepool_extract_cmd::InspectionNamespace::Constructor
                            }
                            IdentifierNamespace::Field => {
                                tidepool_extract_cmd::InspectionNamespace::Field
                            }
                        },
                        name: query.name.clone(),
                    });
                }
                InspectionQuery::Info(name) => {
                    command.inspect_info(name);
                }
                InspectionQuery::TypeSearch(query) => {
                    command.inspect_search(query);
                }
                InspectionQuery::ScopeBrowse => {
                    command.inspect_scope_browse();
                }
                InspectionQuery::Browse { module, expanded } => {
                    command.inspect_browse(module, *expanded);
                }
                InspectionQuery::StructuredInfo { query, provenance } => {
                    command.inspect_structured_info(structured_request(query, provenance));
                }
                InspectionQuery::StructuredType { query, provenance } => {
                    command.inspect_structured_type(structured_request(query, provenance));
                }
            }
        }
    }
    if let Some(batch) = hosted {
        command.hosted_lookup(batch.clone());
    }
    let mut batch_source = None;
    if let Some(expressions) = type_batch {
        let batch_dir = temp.path().join("type-batch");
        std::fs::create_dir(&batch_dir)?;
        let batch_path = batch_dir.join("Expr.hs");
        let mut source = assemble_inspection_module(request.preamble, &imports, &expressions);
        if let Some(effects) = request.effects {
            source.push_str("\n__tidepool_lookup_row :: Data.Proxy.Proxy (");
            source.push_str(effects.expression());
            source.push_str(")\n__tidepool_lookup_row = Data.Proxy.Proxy\n");
        }
        std::fs::write(&batch_path, &source)?;
        sources.insert(batch_path.clone(), source);
        batch_source = Some(batch_path.clone());
        command.inspect_type_batch(&batch_path);
    }

    let endpoint = tidepool_toolchain::toolchain::AdmittedCompilerEndpoint::from_bound(
        command.bind().map_err(map_spawn)?,
    )
    .map_err(|error| CompileError::ExtractFailed(error.to_string()))?;
    let include = request
        .include
        .iter()
        .map(|path| path.to_path_buf())
        .collect::<Vec<_>>();
    let catalog = inputs
        .map(|inputs| &inputs.catalog)
        .cloned()
        .unwrap_or_default()
        .for_deployment(endpoint.deployment())?;
    let offer = match values {
        Some(values) => {
            let certificates = values
                .iter()
                .map(|value| value.checked_artifact().clone())
                .collect::<Vec<_>>();
            let modules = values
                .iter()
                .map(|value| value.module().module_name())
                .collect::<Vec<_>>();
            command.inject_vals(&modules);
            tidepool_toolchain::artifacts::ModuleCandidateOffer::select_inspection_with_catalog(
                &endpoint,
                &include,
                temp.path(),
                request.exact_context.clone(),
                values
                    .iter()
                    .map(|value| (value.module(), value.bytes_owned().clone()))
                    .collect(),
                &certificates,
                &catalog,
            )?
        }
        None => match request.exact_context.clone() {
            Some(context) => {
                tidepool_toolchain::artifacts::ModuleCandidateOffer::select_in_context_with_catalog(
                    &endpoint,
                    &include,
                    temp.path(),
                    context,
                    &catalog,
                )?
            }
            None => {
                tidepool_toolchain::artifacts::ModuleCandidateOffer::select_admitted_with_catalog(
                    &endpoint,
                    &include,
                    temp.path(),
                    &catalog,
                )?
            }
        },
    };
    if let Some(root) = offer.checked_value_root() {
        command.session_root(root);
    }
    crate::paths::apply_admitted_build_products_dir(&mut command, &endpoint);
    let offer = offer.apply_to(command, temp.path())?;
    let command = offer.command();
    #[cfg(test)]
    if std::env::var("TIDEPOOL_KEEP_TEST_LOGS").as_deref() == Ok("1") {
        if let Some(root) = std::env::var_os("TIDEPOOL_TEST_ARTIFACT_ROOT") {
            let retain = || -> std::io::Result<PathBuf> {
                let root = PathBuf::from(root).join("inspection-inputs");
                std::fs::create_dir_all(&root)?;
                let retained = TempDir::new_in(root)?;
                std::fs::write(
                    retained.path().join("worker-request.cbor"),
                    command.request_bytes(),
                )?;
                let mut source_bytes = Vec::new();
                ciborium::ser::into_writer(&sources.iter().collect::<Vec<_>>(), &mut source_bytes)
                    .map_err(|error| std::io::Error::other(error.to_string()))?;
                std::fs::write(retained.path().join("generated-sources.cbor"), source_bytes)?;
                if let Some(scope) = offer.exact_scope_path() {
                    std::fs::copy(scope, retained.path().join("exact-scope.cbor"))?;
                }
                Ok(retained.keep())
            };
            match retain() {
                Ok(path) => eprintln!("owned inspection inputs retained at {}", path.display()),
                Err(error) => eprintln!("could not retain owned inspection inputs: {error}"),
            }
        }
    }
    let run =
        endpoint.execute_with_input_files(&command, offer.input_transport_files(), |close| {
            settlement(close)
        })?;
    timing::record_stage(
        timing::NO_NODE,
        timing::NO_ROUND,
        timing::STAGE_EXTRACT_SPAWN,
        run.elapsed,
        0,
    );
    crate::diag::decode_extract_result(run.success(), &run.output.stdout, &run.output.stderr)
        .map_err(|error| offer.retain_failure(temp.path(), &command, &run.output.stderr, error))?;
    let bytes = std::fs::read(&output_path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            CompileError::MissingOutput(output_path.clone())
        } else {
            CompileError::Io(error)
        }
    })?;
    let output = decode_inspection_output(&bytes)?;
    let successes = match (&output, hosted) {
        (InspectionOutput::Complete(results), None) if results.len() == request.queries.len() => {
            results
                .iter()
                .map(|result| !matches!(result, InspectionResult::Rejected { .. }))
                .collect::<Vec<_>>()
        }
        (InspectionOutput::Hosted(output), Some(batch)) => {
            output.validate_for(batch)?;
            output
                .answers
                .iter()
                .map(|answer| !matches!(answer, HostedLookupAnswer::Rejected { .. }))
                .collect()
        }
        _ => {
            return Err(invalid(
                "inspection output kind or operation coverage differs from the submitted batch",
            ))
        }
    };
    #[cfg(test)]
    if std::env::var("TIDEPOOL_KEEP_TEST_LOGS").as_deref() == Ok("1") {
        if let Some(root) = std::env::var_os("TIDEPOOL_TEST_ARTIFACT_ROOT") {
            let retain = || -> std::io::Result<PathBuf> {
                let root = PathBuf::from(root).join("inspection-receipts");
                std::fs::create_dir_all(&root)?;
                let retained = TempDir::new_in(root)?;
                std::fs::copy(&output_path, retained.path().join("inspection.cbor"))?;
                let directories = sources
                    .keys()
                    .filter_map(|path| path.parent())
                    .collect::<BTreeSet<_>>();
                for (index, directory) in directories.into_iter().enumerate() {
                    let originals = directory.join(".exact-compilations");
                    if !originals.try_exists()? {
                        continue;
                    }
                    let destination = retained.path().join(index.to_string());
                    std::fs::create_dir(&destination)?;
                    std::fs::write(
                        destination.join("original-directory.txt"),
                        originals.as_os_str().as_encoded_bytes(),
                    )?;
                    for entry in std::fs::read_dir(originals)? {
                        let entry = entry?;
                        if !entry.file_type()?.is_dir() {
                            continue;
                        }
                        let snapshot = destination.join(entry.file_name());
                        std::fs::create_dir(&snapshot)?;
                        for name in ["source.hs", "receipt.cbor"] {
                            std::fs::copy(entry.path().join(name), snapshot.join(name))?;
                        }
                        let bytes = std::fs::read(snapshot.join("receipt.cbor"))?;
                        if let Ok(CborValue::Array(header)) =
                            ciborium::de::from_reader(bytes.as_slice())
                        {
                            if let Some(CborValue::Text(evidence)) = header.get(7) {
                                std::fs::write(snapshot.join("dependencies.json"), evidence)?;
                            }
                        }
                    }
                }
                Ok(retained.keep())
            };
            match retain() {
                Ok(path) => eprintln!("owned inspection receipts retained at {}", path.display()),
                Err(error) => eprintln!("could not retain owned inspection receipts: {error}"),
            }
        }
    }
    if offer.exact_scope_path().is_some() && successes.iter().any(|success| *success) {
        let validated = (|| {
            let directories = sources
                .keys()
                .filter_map(|path| path.parent())
                .collect::<BTreeSet<_>>();
            let mut admitted = BTreeSet::<PathBuf>::new();
            for directory in directories {
                if !directory.join(".exact-compilations").try_exists()? {
                    continue;
                }
                for witness in offer.validate_exact_outputs(directory)? {
                    let path = witness.source_path();
                    let source = sources.get(path).ok_or_else(|| {
                        invalid("inspection receipt names another source transaction")
                    })?;
                    if !witness.matches_source(path, source) {
                        return Err(invalid(
                            "inspection receipt does not match submitted source",
                        ));
                    }
                    admitted.insert(path.to_path_buf());
                }
            }
            let batch_admitted = batch_source
                .as_ref()
                .is_some_and(|path| admitted.contains(path));
            for (index, success) in successes.iter().enumerate() {
                if *success && !admitted.contains(&query_sources[index]) && !batch_admitted {
                    return Err(invalid(
                        "inspection result lacks its exact consumed source receipt",
                    ));
                }
            }
            Ok::<_, CompileError>(())
        })();
        validated.map_err(|error| {
            offer.retain_failure(temp.path(), &command, &run.output.stderr, error)
        })?;
    }
    Ok(match output {
        InspectionOutput::Complete(results) => InspectionOutput::Complete(
            request
                .queries
                .iter()
                .zip(results)
                .map(|(query, result)| match (query, result) {
                    (
                        InspectionQuery::StructuredType { .. },
                        InspectionResult::StructuredInfo(Err(error)),
                    ) => InspectionResult::StructuredType(Err(error)),
                    (_, result) => result,
                })
                .collect(),
        ),
        hosted @ InspectionOutput::Hosted(_) => hosted,
    })
}

fn structured_request(
    query: &NameQuery,
    provenance: &ScopeProvenance,
) -> tidepool_extract_cmd::StructuredInspection {
    let exact = exact_name_request(query);
    tidepool_extract_cmd::StructuredInspection {
        scope: exact.scope,
        namespace: exact.namespace,
        name: exact.name,
        generation: provenance.generation,
        fingerprint: provenance.fingerprint.clone(),
    }
}

fn exact_name_request(query: &NameQuery) -> tidepool_extract_cmd::ExactNameInspection {
    tidepool_extract_cmd::ExactNameInspection {
        scope: request_scope(&query.scope),
        namespace: match query.namespace {
            NameNamespace::Any => tidepool_extract_cmd::InspectionNamespace::Any,
            NameNamespace::Value => tidepool_extract_cmd::InspectionNamespace::Value,
            NameNamespace::Type => tidepool_extract_cmd::InspectionNamespace::Type,
            NameNamespace::Constructor => tidepool_extract_cmd::InspectionNamespace::Constructor,
        },
        name: query.name.clone(),
    }
}

fn request_scope(scope: &NameScope) -> tidepool_extract_cmd::InspectionScope {
    match scope {
        NameScope::Current => tidepool_extract_cmd::InspectionScope::Current,
        NameScope::PublicModule(module) => {
            tidepool_extract_cmd::InspectionScope::PublicModule(module.clone())
        }
    }
}

fn map_spawn(error: SpawnError) -> CompileError {
    CompileError::Io(crate::extract_spawn_error(error.source))
}

fn decode_inspections(bytes: &[u8]) -> Result<Vec<InspectionResult>, CompileError> {
    match decode_inspection_output(bytes)? {
        InspectionOutput::Complete(results) => Ok(results),
        InspectionOutput::Hosted(_) => Err(invalid(
            "hosted lookup output cannot satisfy a complete inspection",
        )),
    }
}

fn decode_inspection_output(bytes: &[u8]) -> Result<InspectionOutput, CompileError> {
    let mut reader = std::io::Cursor::new(bytes);
    let value: CborValue = ciborium::de::from_reader(&mut reader)
        .map_err(|error| invalid(format!("malformed CBOR: {error}")))?;
    if reader.position() != bytes.len() as u64 {
        return Err(invalid("trailing CBOR data"));
    }
    let root = array_len(&value, 2, "receipt")?;
    if text(&root[0], "version")? != "TPINSP008" {
        return Err(invalid("unsupported receipt version"));
    }
    let body = array(&root[1], "inspection output")?;
    match body
        .first()
        .ok_or_else(|| invalid("empty inspection output"))
        .and_then(|v| text(v, "output kind"))?
    {
        "Complete" => {
            let fields = array_len(&root[1], 2, "complete output")?;
            Ok(InspectionOutput::Complete(
                array(&fields[1], "results")?
                    .iter()
                    .map(decode_inspection_result)
                    .collect::<Result<_, _>>()?,
            ))
        }
        "HostedLookup" => {
            let fields = array_len(&root[1], 3, "hosted output")?;
            Ok(InspectionOutput::Hosted(HostedLookupOutput {
                answers: array(&fields[1], "hosted answers")?
                    .iter()
                    .map(decode_hosted_answer)
                    .collect::<Result<_, _>>()?,
                discovery: match &fields[2] {
                    CborValue::Null => None,
                    value => Some(decode_hosted_discovery(value)?),
                },
            }))
        }
        other => Err(invalid(format!("unknown inspection output kind {other:?}"))),
    }
}

fn ordinal(value: &CborValue) -> Result<u32, CompileError> {
    u32::try_from(unsigned(value, "operation ordinal")?)
        .map_err(|_| invalid("operation ordinal exceeds u32"))
}

fn count(value: &CborValue, what: &str) -> Result<usize, CompileError> {
    usize::try_from(unsigned(value, what)?).map_err(|_| invalid(format!("{what} exceeds usize")))
}

fn decode_selected<T>(
    value: &CborValue,
    decode: fn(&CborValue) -> Result<T, CompileError>,
) -> Result<SelectedMatches<T>, CompileError> {
    let fields = array_len(value, 3, "selected matches")?;
    let observed = count(&fields[0], "observed matches")?;
    let limit = count(&fields[1], "match limit")?;
    let retained = array(&fields[2], "retained matches")?;
    if limit != 20 || retained.len() != observed.min(limit) {
        return Err(invalid(
            "selected matches have inconsistent observed count or limit",
        ));
    }
    let entries = retained.iter().map(decode).collect::<Result<Vec<_>, _>>()?;
    Ok(SelectedMatches {
        entries,
        observed,
        limit,
    })
}

fn decode_selected_lookup(value: &CborValue) -> Result<SelectedLookupAnswer, CompileError> {
    let body = array(value, "selected lookup")?;
    let tag = body
        .first()
        .ok_or_else(|| invalid("empty selected lookup"))
        .and_then(|v| text(v, "selected lookup tag"))?;
    match tag {
        "Info" | "Ambiguous" => {
            let row = array_len(value, 2, "selected name")?;
            let selected = decode_selected(&row[1], decode_info_entry)?;
            if (tag == "Info" && selected.observed != 1)
                || (tag == "Ambiguous" && selected.observed < 2)
            {
                return Err(invalid(
                    "selected name status disagrees with complete match count",
                ));
            }
            Ok(if tag == "Info" {
                SelectedLookupAnswer::Info(selected)
            } else {
                SelectedLookupAnswer::Ambiguous(selected)
            })
        }
        "Module" => {
            let row = array_len(value, 3, "selected module")?;
            Ok(SelectedLookupAnswer::Module {
                module: text(&row[1], "selected module")?.into(),
                matches: decode_selected(&row[2], decode_info_entry)?,
            })
        }
        "Missing" => {
            let row = array_len(value, 2, "selected miss")?;
            Ok(SelectedLookupAnswer::Missing {
                export_names: decode_texts(&row[1], "nearby names")?,
            })
        }
        "ModuleNotFound" => {
            let row = array_len(value, 2, "selected missing module")?;
            Ok(SelectedLookupAnswer::ModuleNotFound(
                text(&row[1], "missing module")?.into(),
            ))
        }
        other => Err(invalid(format!("unknown selected lookup {other:?}"))),
    }
}

fn validate_hosted_output(
    batch: &HostedLookupBatch,
    output: &HostedLookupOutput,
) -> Result<(), CompileError> {
    if batch.operations.len() != output.answers.len() {
        return Err(invalid(
            "hosted output omitted or added a physical operation",
        ));
    }
    let mut named_miss = false;
    for (operation, answer) in batch.operations.iter().zip(&output.answers) {
        let valid = match (operation, answer) {
            (
                HostedLookupOperation::Primary {
                    ordinal,
                    query: HostedLookupQuery::Name(_),
                    ..
                },
                HostedLookupAnswer::Name {
                    ordinal: returned,
                    answer,
                    ..
                },
            ) => {
                named_miss |= matches!(
                    answer,
                    SelectedLookupAnswer::Missing { .. } | SelectedLookupAnswer::ModuleNotFound(_)
                );
                ordinal == returned
            }
            (
                HostedLookupOperation::Primary {
                    ordinal,
                    query: HostedLookupQuery::TypeSearch(_),
                    ..
                },
                HostedLookupAnswer::Type {
                    ordinal: returned, ..
                },
            ) => ordinal == returned,
            (
                HostedLookupOperation::ExactReference { destinations, .. },
                HostedLookupAnswer::Reference {
                    destinations: returned,
                    ..
                },
            ) => destinations == returned,
            (
                HostedLookupOperation::Primary { ordinal, .. },
                HostedLookupAnswer::Rejected {
                    operation: HostedOperationIdentity::Primary(returned),
                    ..
                },
            ) => ordinal == returned,
            (
                HostedLookupOperation::ExactReference { destinations, .. },
                HostedLookupAnswer::Rejected {
                    operation: HostedOperationIdentity::Reference(returned),
                    ..
                },
            ) => destinations == returned,
            _ => false,
        };
        if !valid {
            return Err(invalid(
                "hosted answer belongs to another operation or ordinal domain",
            ));
        }
    }
    match (&batch.discovery, &output.discovery) {
        (None, None) => {}
        (Some(policy), Some(selected)) => {
            if selected.candidate_limit != policy.candidate_limit as usize
                || selected.eligible_modules != policy.eligible_modules as usize
            {
                return Err(invalid(
                    "hosted discovery frontier differs from submitted policy",
                ));
            }
            let scopes = match &selected.fallback {
                HostedFallback::Attempted(scopes) => {
                    if policy.candidate_limit == 0 || !named_miss {
                        return Err(invalid(
                            "fallback search has no genuine successful named miss",
                        ));
                    }
                    if selected.submitted_modules
                        != policy
                            .scopes
                            .iter()
                            .filter(|scope| {
                                matches!(
                                    scope,
                                    tidepool_extract_cmd::InspectionScope::PublicModule(_)
                                )
                            })
                            .count()
                    {
                        return Err(invalid(
                            "fallback submission count differs from admitted scopes",
                        ));
                    }
                    scopes.as_slice()
                }
                HostedFallback::ZeroBudget => {
                    if policy.candidate_limit != 0
                        || !selected.candidates.is_empty()
                        || selected.enumerated_occurrences != 0
                    {
                        return Err(invalid(
                            "zero discovery budget performed candidate enumeration",
                        ));
                    }
                    &[]
                }
                HostedFallback::NotNeeded => {
                    if policy.candidate_limit == 0 || named_miss {
                        return Err(invalid("required fallback was reported unneeded"));
                    }
                    &[]
                }
                HostedFallback::Unavailable(_) => {
                    if policy.candidate_limit == 0 {
                        return Err(invalid("zero budget requires its explicit fallback state"));
                    }
                    &[]
                }
            };
            if !matches!(selected.fallback, HostedFallback::Attempted(_))
                && selected.submitted_modules != 0
            {
                return Err(invalid("unattempted fallback claims submitted modules"));
            }
            let mut cursor = 0;
            for observed in scopes {
                let Some(offset) =
                    policy.scopes[cursor..]
                        .iter()
                        .position(|planned| match (planned, observed) {
                            (
                                tidepool_extract_cmd::InspectionScope::Current,
                                NameScope::Current,
                            ) => true,
                            (
                                tidepool_extract_cmd::InspectionScope::PublicModule(left),
                                NameScope::PublicModule(right),
                            ) => left == right,
                            _ => false,
                        })
                else {
                    return Err(invalid(
                        "fallback receipt has an unsubmitted, repeated or reordered scope",
                    ));
                };
                cursor += offset + 1;
            }
            let origins = batch
                .operations
                .iter()
                .filter_map(|operation| match operation {
                    HostedLookupOperation::Primary { origin, .. } => Some(origin),
                    _ => None,
                })
                .collect::<Vec<_>>();
            for (index, candidate) in selected.candidates.iter().enumerate() {
                if candidate.origins.is_empty()
                    || candidate
                        .origins
                        .iter()
                        .any(|origin| !origins.contains(&origin))
                    || selected.candidates[..index].iter().any(|previous| {
                        previous.query == candidate.query
                            && previous.reference == candidate.reference
                    })
                {
                    return Err(invalid(
                        "candidate identity or origins do not belong to this hosted batch",
                    ));
                }
            }
        }
        _ => {
            return Err(invalid(
                "hosted output changed whether discovery was requested",
            ))
        }
    }
    Ok(())
}

fn reference_destinations(value: &CborValue) -> Result<Vec<u32>, CompileError> {
    let destinations = array(value, "reference destinations")?
        .iter()
        .map(ordinal)
        .collect::<Result<Vec<_>, _>>()?;
    if destinations.is_empty()
        || destinations.iter().copied().collect::<BTreeSet<_>>().len() != destinations.len()
    {
        return Err(invalid("invalid reference destination identities"));
    }
    Ok(destinations)
}

fn decode_hosted_answer(value: &CborValue) -> Result<HostedLookupAnswer, CompileError> {
    let body = array(value, "hosted answer")?;
    let tag = body
        .first()
        .ok_or_else(|| invalid("empty hosted answer"))
        .and_then(|v| text(v, "hosted answer tag"))?;
    match tag {
        "Name" => {
            let row = array_len(value, 4, "hosted name")?;
            Ok(HostedLookupAnswer::Name {
                ordinal: ordinal(&row[1])?,
                scopes: decode_scopes(&row[2])?,
                answer: decode_selected_lookup(&row[3])?,
            })
        }
        "Type" => {
            let row = array_len(value, 3, "hosted type")?;
            Ok(HostedLookupAnswer::Type {
                ordinal: ordinal(&row[1])?,
                matches: decode_selected(&row[2], decode_type_match)?,
            })
        }
        "Reference" => {
            let row = array_len(value, 4, "hosted reference")?;
            Ok(HostedLookupAnswer::Reference {
                destinations: reference_destinations(&row[1])?,
                scopes: decode_scopes(&row[2])?,
                answer: decode_selected_lookup(&row[3])?,
            })
        }
        "Rejected" => {
            let row = array_len(value, 3, "hosted rejection")?;
            let identity = array_len(&row[1], 2, "rejected operation")?;
            let operation = match text(&identity[0], "rejected operation kind")? {
                "Primary" => HostedOperationIdentity::Primary(ordinal(&identity[1])?),
                "Reference" => {
                    HostedOperationIdentity::Reference(reference_destinations(&identity[1])?)
                }
                other => return Err(invalid(format!("unknown rejected operation {other:?}"))),
            };
            Ok(HostedLookupAnswer::Rejected {
                operation,
                diagnostic: text(&row[2], "hosted diagnostic")?.into(),
            })
        }
        other => Err(invalid(format!("unknown hosted answer {other:?}"))),
    }
}

fn decode_hosted_discovery(value: &CborValue) -> Result<HostedDiscoverySelection, CompileError> {
    let row = array_len(value, 9, "hosted discovery")?;
    let candidate_rows = array(&row[0], "selected candidates")?;
    if candidate_rows.len() > 128 {
        return Err(invalid("hosted candidate count exceeds its maximum budget"));
    }
    let candidates = candidate_rows
        .iter()
        .map(|value| {
            let fields = array_len(value, 5, "discovery candidate")?;
            Ok(HostedDiscoveryCandidate {
                query: text(&fields[0], "candidate query")?.into(),
                origins: decode_texts(&fields[1], "candidate origins")?,
                summary: text(&fields[2], "complete candidate summary")?.into(),
                local: boolean(&fields[3], "candidate locality")?,
                reference: match &fields[4] {
                    CborValue::Null => None,
                    value => Some(decode_identifier_ref(value)?),
                },
            })
        })
        .collect::<Result<Vec<_>, CompileError>>()?;
    let fallback = array(&row[1], "fallback observation")?;
    let fallback = match fallback
        .first()
        .ok_or_else(|| invalid("empty fallback observation"))
        .and_then(|v| text(v, "fallback kind"))?
    {
        "NotNeeded" => {
            array_len(&row[1], 1, "unneeded fallback")?;
            HostedFallback::NotNeeded
        }
        "ZeroBudget" => {
            array_len(&row[1], 1, "zero-budget fallback")?;
            HostedFallback::ZeroBudget
        }
        "Unavailable" => {
            let fields = array_len(&row[1], 2, "unavailable fallback")?;
            HostedFallback::Unavailable(text(&fields[1], "fallback failure")?.into())
        }
        "Attempted" => {
            let fields = array_len(&row[1], 2, "attempted fallback")?;
            HostedFallback::Attempted(decode_scopes(&fields[1])?)
        }
        other => return Err(invalid(format!("unknown fallback observation {other:?}"))),
    };
    let selected = HostedDiscoverySelection {
        candidates,
        fallback,
        eligible_modules: count(&row[2], "eligible modules")?,
        submitted_modules: count(&row[3], "submitted modules")?,
        enumerated_occurrences: count(&row[4], "enumerated occurrences")?,
        discarded_occurrences: count(&row[5], "discarded occurrences")?,
        candidate_limit: count(&row[6], "candidate limit")?,
        issue: match &row[7] {
            CborValue::Null => None,
            value => Some(text(value, "discovery issue")?.into()),
        },
    };
    if count(&row[8], "module limit")? != 128
        || selected.candidate_limit > 128
        || selected.candidates.len() > selected.candidate_limit
        || selected.submitted_modules > selected.eligible_modules.min(128)
        || selected.discarded_occurrences > selected.enumerated_occurrences
        || selected.candidates.len()
            > selected.enumerated_occurrences - selected.discarded_occurrences
    {
        return Err(invalid("inconsistent discovery frontier counts"));
    }
    Ok(selected)
}

fn decode_inspection_result(value: &CborValue) -> Result<InspectionResult, CompileError> {
    let body = array(value, "body")?;
    let tag = body
        .first()
        .ok_or_else(|| invalid("empty result body"))
        .and_then(|value| text(value, "result tag"))?;
    match tag {
        "Lookup" => {
            let body = array_len(value, 4, "Lookup result")?;
            Ok(InspectionResult::Lookup {
                query: text(&body[1], "Lookup query")?.into(),
                scopes: decode_scopes(&body[2])?,
                answer: decode_lookup_answer(&body[3])?,
            })
        }
        "Type" => {
            let body = array_len(value, 4, "Type result")?;
            Ok(InspectionResult::Type {
                expression: text(&body[1], "Type expression")?.into(),
                display: text(&body[2], "Type display")?.into(),
                availability: InspectionAvailability::decode(&body[3], "Type availability")?,
            })
        }
        "Info" => {
            let body = array_len(value, 3, "Info result")?;
            let entries = array(&body[2], "Info entries")?
                .iter()
                .map(decode_info_entry)
                .collect::<Result<_, _>>()?;
            Ok(InspectionResult::Info {
                query: text(&body[1], "Info query")?.into(),
                entries,
            })
        }
        "Ambiguous" => {
            let body = array_len(value, 3, "Ambiguous result")?;
            let entries = array(&body[2], "Ambiguous entries")?
                .iter()
                .map(decode_info_entry)
                .collect::<Result<_, _>>()?;
            Ok(InspectionResult::Ambiguous {
                query: text(&body[1], "Ambiguous query")?.into(),
                entries,
            })
        }
        "NotFound" => {
            let body = array_len(value, 2, "NotFound result")?;
            Ok(InspectionResult::NotFound {
                query: text(&body[1], "NotFound query")?.into(),
            })
        }
        "ModuleNotFound" => {
            let body = array_len(value, 3, "ModuleNotFound result")?;
            Ok(InspectionResult::ModuleNotFound {
                module: text(&body[1], "ModuleNotFound module")?.into(),
                scopes: decode_scopes(&body[2])?,
            })
        }
        "Rejected" => {
            let body = array_len(value, 2, "Rejected result")?;
            Ok(InspectionResult::Rejected {
                diagnostic: text(&body[1], "Rejected diagnostic")?.into(),
            })
        }
        "Browse" => {
            let body = array_len(value, 4, "Browse result")?;
            let entries = array(&body[3], "Browse entries")?
                .iter()
                .map(decode_info_entry)
                .collect::<Result<_, _>>()?;
            Ok(InspectionResult::Browse {
                module: text(&body[1], "Browse module")?.into(),
                expanded: boolean(&body[2], "Browse expanded")?,
                entries,
            })
        }
        "TypeMatches" => {
            let body = array_len(value, 3, "TypeMatches result")?;
            let matches = array(&body[2], "TypeMatches matches")?
                .iter()
                .map(decode_type_match)
                .collect::<Result<_, _>>()?;
            Ok(InspectionResult::TypeMatches {
                query: text(&body[1], "TypeMatches query")?.into(),
                matches,
            })
        }
        "StructuredInfoOk" => {
            let body = array_len(value, 2, "StructuredInfoOk result")?;
            Ok(InspectionResult::StructuredInfo(Ok(Box::new(
                decode_identifier_info(&body[1])?,
            ))))
        }
        "StructuredTypeOk" => {
            let body = array_len(value, 2, "StructuredTypeOk result")?;
            Ok(InspectionResult::StructuredType(Ok(decode_type_info(
                &body[1],
            )?)))
        }
        "StructuredError" => {
            let body = array_len(value, 2, "StructuredError result")?;
            Ok(InspectionResult::StructuredInfo(Err(decode_query_error(
                &body[1],
            )?)))
        }
        other => Err(invalid(format!("unknown result tag {other:?}"))),
    }
}

fn decode_identifier_info(value: &CborValue) -> Result<IdentifierInfo, CompileError> {
    let fields = array_len(value, 4, "IdentifierInfo")?;
    Ok(IdentifierInfo {
        identifier: decode_identifier_ref(&fields[0])?,
        declaration: decode_declaration(&fields[1])?,
        parent: match &fields[2] {
            CborValue::Null => None,
            value => Some(decode_identifier_ref(value)?),
        },
        provenance: decode_provenance(&fields[3])?,
    })
}

fn decode_type_info(value: &CborValue) -> Result<TypeInfo, CompileError> {
    let fields = array_len(value, 3, "TypeInfo")?;
    Ok(TypeInfo {
        identifier: decode_identifier_ref(&fields[0])?,
        expression: decode_type_expression(&fields[1])?,
        provenance: decode_provenance(&fields[2])?,
    })
}

fn decode_identifier_ref(value: &CborValue) -> Result<IdentifierRef, CompileError> {
    let fields = array_len(value, 3, "IdentifierRef")?;
    let namespace = match text(&fields[2], "IdentifierRef namespace")? {
        "Value" => IdentifierNamespace::Value,
        "Type" => IdentifierNamespace::Type,
        "Constructor" => IdentifierNamespace::Constructor,
        "Field" => IdentifierNamespace::Field,
        other => {
            return Err(invalid(format!(
                "unknown IdentifierRef namespace {other:?}"
            )));
        }
    };
    Ok(IdentifierRef {
        module: text(&fields[0], "IdentifierRef module")?.into(),
        name: text(&fields[1], "IdentifierRef name")?.into(),
        namespace,
    })
}

fn decode_type_expression(value: &CborValue) -> Result<TypeExpression, CompileError> {
    let fields = array_len(value, 3, "TypeExpression")?;
    Ok(TypeExpression {
        canonical: text(&fields[0], "TypeExpression canonical")?.into(),
        variables: decode_texts(&fields[1], "TypeExpression variables")?,
        constraints: decode_texts(&fields[2], "TypeExpression constraints")?,
    })
}

fn decode_provenance(value: &CborValue) -> Result<ScopeProvenance, CompileError> {
    let fields = array_len(value, 3, "ScopeProvenance")?;
    Ok(ScopeProvenance {
        scope: decode_scope(&fields[0])?,
        generation: unsigned(&fields[1], "ScopeProvenance generation")?,
        fingerprint: text(&fields[2], "ScopeProvenance fingerprint")?.into(),
    })
}

fn decode_query_error(value: &CborValue) -> Result<QueryError, CompileError> {
    let fields = array(value, "StructuredError")?;
    let tag = fields
        .first()
        .ok_or_else(|| invalid("empty StructuredError"))
        .and_then(|value| text(value, "StructuredError tag"))?;
    match tag {
        "Unknown" => {
            let fields = array_len(value, 2, "Unknown error")?;
            Ok(QueryError::Unknown(decode_query(&fields[1])?))
        }
        "Ambiguous" => {
            let fields = array_len(value, 3, "Ambiguous error")?;
            Ok(QueryError::Ambiguous(
                decode_query(&fields[1])?,
                array(&fields[2], "Ambiguous candidates")?
                    .iter()
                    .map(decode_identifier_ref)
                    .collect::<Result<_, _>>()?,
            ))
        }
        "UnknownModule" => {
            let fields = array_len(value, 2, "UnknownModule error")?;
            Ok(QueryError::UnknownModule(
                text(&fields[1], "UnknownModule module")?.into(),
            ))
        }
        "Unsupported" => {
            let fields = array_len(value, 2, "Unsupported error")?;
            Ok(QueryError::Unsupported(
                text(&fields[1], "Unsupported detail")?.into(),
            ))
        }
        other => Err(invalid(format!("unknown StructuredError tag {other:?}"))),
    }
}

fn decode_query(value: &CborValue) -> Result<NameQuery, CompileError> {
    let fields = array_len(value, 3, "Structured query")?;
    let namespace = match text(&fields[1], "Structured query namespace")? {
        "Any" => NameNamespace::Any,
        "Value" => NameNamespace::Value,
        "Type" => NameNamespace::Type,
        "Constructor" => NameNamespace::Constructor,
        other => {
            return Err(invalid(format!(
                "unknown structured query namespace {other:?}"
            )));
        }
    };
    Ok(NameQuery {
        scope: decode_scope(&fields[0])?,
        namespace,
        name: text(&fields[2], "Structured query name")?.into(),
    })
}

fn decode_scope(value: &CborValue) -> Result<NameScope, CompileError> {
    let fields = array(value, "scope")?;
    match fields
        .first()
        .and_then(|value| text(value, "scope tag").ok())
    {
        Some("Current") if fields.len() == 1 => Ok(NameScope::Current),
        Some("PublicModule") if fields.len() == 2 => Ok(NameScope::PublicModule(
            text(&fields[1], "PublicModule name")?.into(),
        )),
        Some(tag) => Err(invalid(format!("invalid scope {tag:?}"))),
        None => Err(invalid("scope tag must be text")),
    }
}

fn decode_declaration(value: &CborValue) -> Result<DeclarationInfo, CompileError> {
    let fields = array(value, "DeclarationInfo")?;
    let tag = fields
        .first()
        .ok_or_else(|| invalid("empty DeclarationInfo"))
        .and_then(|value| text(value, "DeclarationInfo tag"))?;
    match tag {
        "Value" => Ok(DeclarationInfo::Value(decode_type_expression(
            &array_len(value, 2, "Value declaration")?[1],
        )?)),
        "Data" => {
            let f = array_len(value, 3, "Data declaration")?;
            Ok(DeclarationInfo::Data {
                parameters: decode_texts(&f[1], "Data parameters")?,
                constructors: array(&f[2], "Data constructors")?
                    .iter()
                    .map(decode_constructor)
                    .collect::<Result<_, _>>()?,
            })
        }
        "Newtype" => {
            let f = array_len(value, 3, "Newtype declaration")?;
            Ok(DeclarationInfo::Newtype {
                parameters: decode_texts(&f[1], "Newtype parameters")?,
                constructor: decode_constructor(&f[2])?,
            })
        }
        "TypeSynonym" => {
            let f = array_len(value, 3, "TypeSynonym declaration")?;
            Ok(DeclarationInfo::TypeSynonym {
                parameters: decode_texts(&f[1], "TypeSynonym parameters")?,
                body: decode_type_expression(&f[2])?,
            })
        }
        "Class" => {
            let f = array_len(value, 4, "Class declaration")?;
            Ok(DeclarationInfo::Class {
                parameters: decode_texts(&f[1], "Class parameters")?,
                superclasses: array(&f[2], "Class superclasses")?
                    .iter()
                    .map(decode_type_expression)
                    .collect::<Result<_, _>>()?,
                methods: array(&f[3], "Class methods")?
                    .iter()
                    .map(decode_class_method)
                    .collect::<Result<_, _>>()?,
            })
        }
        "Constructor" => {
            let f = array_len(value, 3, "Constructor declaration")?;
            Ok(DeclarationInfo::Constructor {
                parent: decode_identifier_ref(&f[1])?,
                constructor: decode_constructor(&f[2])?,
            })
        }
        "RecordSelector" => {
            let f = array_len(value, 3, "RecordSelector declaration")?;
            Ok(DeclarationInfo::RecordSelector {
                parent: decode_identifier_ref(&f[1])?,
                ty: decode_type_expression(&f[2])?,
            })
        }
        other => Err(invalid(format!("unknown DeclarationInfo tag {other:?}"))),
    }
}

fn decode_constructor(value: &CborValue) -> Result<ConstructorInfo, CompileError> {
    let fields = array_len(value, 4, "ConstructorInfo")?;
    Ok(ConstructorInfo {
        identifier: decode_identifier_ref(&fields[0])?,
        ty: decode_type_expression(&fields[1])?,
        arguments: array(&fields[2], "ConstructorInfo arguments")?
            .iter()
            .map(decode_type_expression)
            .collect::<Result<_, _>>()?,
        fields: array(&fields[3], "ConstructorInfo fields")?
            .iter()
            .map(decode_field)
            .collect::<Result<_, _>>()?,
    })
}

fn decode_field(value: &CborValue) -> Result<FieldInfo, CompileError> {
    let fields = array_len(value, 2, "FieldInfo")?;
    Ok(FieldInfo {
        name: text(&fields[0], "FieldInfo name")?.into(),
        ty: decode_type_expression(&fields[1])?,
    })
}

fn decode_class_method(value: &CborValue) -> Result<ClassMethodInfo, CompileError> {
    let fields = array_len(value, 2, "ClassMethodInfo")?;
    Ok(ClassMethodInfo {
        identifier: decode_identifier_ref(&fields[0])?,
        ty: decode_type_expression(&fields[1])?,
    })
}

fn decode_texts(value: &CborValue, what: &str) -> Result<Vec<String>, CompileError> {
    array(value, what)?
        .iter()
        .map(|value| Ok(text(value, what)?.into()))
        .collect()
}

fn unsigned(value: &CborValue, what: &str) -> Result<u64, CompileError> {
    match value {
        CborValue::Integer(value) => {
            u64::try_from(*value).map_err(|_| invalid(format!("{what} must be u64")))
        }
        _ => Err(invalid(format!("{what} must be u64"))),
    }
}

fn decode_type_match(value: &CborValue) -> Result<TypeMatch, CompileError> {
    let fields = array_len(value, 6, "TypeMatch")?;
    let module = match &fields[1] {
        CborValue::Null => None,
        value => Some(text(value, "TypeMatch module")?.into()),
    };
    let quality = match text(&fields[3], "TypeMatch quality")? {
        "Exact" => TypeMatchQuality::Exact,
        "Usable" => TypeMatchQuality::Usable,
        other => return Err(invalid(format!("unknown TypeMatch quality {other:?}"))),
    };
    Ok(TypeMatch {
        name: text(&fields[0], "TypeMatch name")?.into(),
        module,
        signature: text(&fields[2], "TypeMatch signature")?.into(),
        quality,
        availability: InspectionAvailability::decode(&fields[4], "TypeMatch availability")?,
        references: array(&fields[5], "TypeMatch references")?
            .iter()
            .map(decode_identifier_ref)
            .collect::<Result<_, _>>()?,
    })
}

fn boolean(value: &CborValue, what: &str) -> Result<bool, CompileError> {
    match value {
        CborValue::Bool(value) => Ok(*value),
        _ => Err(invalid(format!("{what} must be boolean"))),
    }
}

fn decode_scopes(value: &CborValue) -> Result<Vec<NameScope>, CompileError> {
    let scopes = array(value, "searched scopes")?
        .iter()
        .map(decode_scope)
        .collect::<Result<Vec<_>, _>>()?;
    for (index, scope) in scopes.iter().enumerate() {
        if scopes[..index].contains(scope) {
            return Err(invalid("duplicate searched scope"));
        }
    }
    Ok(scopes)
}

fn decode_lookup_answer(value: &CborValue) -> Result<LookupAnswer, CompileError> {
    let fields = array(value, "Lookup answer")?;
    let tag = fields
        .first()
        .ok_or_else(|| invalid("empty Lookup answer"))?;
    let entries = |value| {
        array(value, "Lookup entries")?
            .iter()
            .map(decode_info_entry)
            .collect::<Result<Vec<_>, CompileError>>()
    };
    match text(tag, "Lookup answer tag")? {
        "Info" => {
            let row = array_len(value, 2, "Lookup Info")?;
            Ok(LookupAnswer::Info(entries(&row[1])?))
        }
        "Ambiguous" => {
            let row = array_len(value, 2, "Lookup Ambiguous")?;
            Ok(LookupAnswer::Ambiguous(entries(&row[1])?))
        }
        "Module" => {
            let row = array_len(value, 3, "Lookup Module")?;
            Ok(LookupAnswer::Module {
                module: text(&row[1], "Lookup module")?.into(),
                entries: entries(&row[2])?,
            })
        }
        "Missing" => {
            let row = array_len(value, 2, "Lookup Missing")?;
            Ok(LookupAnswer::Missing {
                export_names: array(&row[1], "Lookup export names")?
                    .iter()
                    .map(|name| text(name, "Lookup export name").map(str::to_owned))
                    .collect::<Result<_, _>>()?,
            })
        }
        "ModuleNotFound" => {
            let row = array_len(value, 2, "Lookup ModuleNotFound")?;
            Ok(LookupAnswer::ModuleNotFound(
                text(&row[1], "Lookup missing module")?.into(),
            ))
        }
        other => Err(invalid(format!("unknown Lookup answer {other:?}"))),
    }
}

fn decode_info_entry(value: &CborValue) -> Result<InfoEntry, CompileError> {
    let fields = array_len(value, 6, "Info entry")?;
    let module = match &fields[1] {
        CborValue::Null => None,
        value => Some(text(value, "Info module")?.into()),
    };
    Ok(InfoEntry {
        name: text(&fields[0], "Info name")?.into(),
        module,
        kind: text(&fields[2], "Info kind")?.into(),
        display: text(&fields[3], "Info display")?.into(),
        availability: InspectionAvailability::decode(&fields[4], "Info availability")?,
        references: array(&fields[5], "Info references")?
            .iter()
            .map(decode_identifier_ref)
            .collect::<Result<_, _>>()?,
    })
}

fn array<'a>(value: &'a CborValue, what: &str) -> Result<&'a [CborValue], CompileError> {
    match value {
        CborValue::Array(values) => Ok(values),
        _ => Err(invalid(format!("{what} must be an array"))),
    }
}

fn array_len<'a>(
    value: &'a CborValue,
    expected: usize,
    what: &str,
) -> Result<&'a [CborValue], CompileError> {
    let values = array(value, what)?;
    if values.len() != expected {
        return Err(invalid(format!(
            "{what} must contain {expected} items, got {}",
            values.len()
        )));
    }
    Ok(values)
}

fn text<'a>(value: &'a CborValue, what: &str) -> Result<&'a str, CompileError> {
    match value {
        CborValue::Text(value) => Ok(value),
        _ => Err(invalid(format!("{what} must be text"))),
    }
}

fn invalid(detail: impl Into<String>) -> CompileError {
    CompileError::ExtractFailed(format!("inspection receipt: {}", detail.into()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn admitted_inspection_refuses_sibling_pairing_with_shared_root_and_context() {
        use crate::session::{ModuleEnv, PersistentSession, SessionId, SessionLib};
        let root = tempfile::tempdir().unwrap();
        let lib =
            SessionLib::open(SessionId(902), root.path(), ModuleEnv::standalone_default()).unwrap();
        let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
        let first_scope = session.mint_isolated_scope();
        let second_scope = session.mint_isolated_scope();
        let first = session.compile_view_in(first_scope).unwrap();
        let second = session.compile_view_in(second_scope).unwrap();
        assert_eq!(first.session_root(), second.session_root());
        assert_eq!(
            first.exact_declaration_context(),
            second.exact_declaration_context()
        );
        assert_eq!(
            first.injected_module_names(),
            second.injected_module_names()
        );
        let inputs =
            AdmittedInspectionInputs::capture(first.clone(), Vec::new(), Default::default());
        let injected = first.injected_module_names();
        let request = || InspectionRequest {
            exact_context: first.exact_compile_context(),
            preamble: "",
            imports: "",
            include: &[],
            session_root: first.session_root(),
            inject_modules: &injected,
            queries: &[],
            effects: None,
        };
        assert!(
            tidepool_testing::with_settlement(|settlement| run_admitted_inspections(
                request(),
                &first,
                &inputs,
                settlement
            ))
            .unwrap()
            .is_empty()
        );
        assert!(
            tidepool_testing::with_settlement(|settlement| run_admitted_inspections(
                request(),
                &second,
                &inputs,
                settlement
            ))
            .is_err()
        );
    }
    use super::*;
    use tidepool_testing::eval_harness;

    #[test]
    fn admitted_empty_snapshot_answers_type_search() {
        use crate::session::{ModuleEnv, PersistentSession, SessionId, SessionLib};
        eval_harness::require_extract();
        let root = tempfile::tempdir().unwrap();
        let include = tempfile::tempdir().unwrap();
        assert!(include.path().is_absolute());
        assert!(std::fs::read_dir(include.path()).unwrap().next().is_none());
        let lib =
            SessionLib::open(SessionId(903), root.path(), ModuleEnv::standalone_default()).unwrap();
        let session = PersistentSession::new(Some(lib), 1024 * 1024);
        let view = session
            .compile_view_in(tidepool_codegen::scope::ScopeId::ROOT)
            .unwrap();
        assert!(view.exact_declaration_context().is_none());
        assert!(view.reachable_values().is_empty());
        let values = session.capture_value_interfaces(&view).unwrap();
        assert!(values.is_empty());
        let inputs = AdmittedInspectionInputs::capture(view.clone(), values, Default::default());
        let queries = [InspectionQuery::TypeSearch("Int -> Int".into())];
        let results = tidepool_testing::with_settlement(|settlement| {
            run_admitted_inspections(
                InspectionRequest {
                    exact_context: None,
                    preamble: "module Expr where\nimport Prelude\n",
                    imports: "",
                    include: &[include.path()],
                    session_root: view.session_root(),
                    inject_modules: &[],
                    queries: &queries,
                    effects: None,
                },
                &view,
                &inputs,
                settlement,
            )
        })
        .expect("fresh empty checked snapshot supports a valid type search");
        assert!(
            matches!(&results[..], [InspectionResult::TypeMatches { matches, .. }]
            if matches.iter().any(|entry| entry.name == "id"
                && entry.module.as_deref() == Some("GHC.Internal.Base")
                && entry.quality == TypeMatchQuality::Usable)),
            "imported Prelude.id must match concrete Int -> Int: {results:#?}"
        );
    }

    fn encoded(value: CborValue) -> Vec<u8> {
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&value, &mut bytes).unwrap();
        bytes
    }

    fn complete_receipt(results: Vec<CborValue>) -> CborValue {
        CborValue::Array(vec![
            CborValue::Text("TPINSP008".into()),
            CborValue::Array(vec![
                CborValue::Text("Complete".into()),
                CborValue::Array(results),
            ]),
        ])
    }

    #[test]
    fn decodes_every_result_shape() {
        let receipt = complete_receipt(vec![
            CborValue::Array(vec![
                CborValue::Text("Type".into()),
                CborValue::Text("fmap".into()),
                CborValue::Text("Functor f => (a -> b) -> f a -> f b".into()),
                CborValue::Text("Unknown".into()),
            ]),
            CborValue::Array(vec![
                CborValue::Text("Info".into()),
                CborValue::Text("Maybe".into()),
                CborValue::Array(vec![CborValue::Array(vec![
                    CborValue::Text("Maybe".into()),
                    CborValue::Text("GHC.Internal.Maybe".into()),
                    CborValue::Text("type".into()),
                    CborValue::Text("data Maybe a = Nothing | Just a".into()),
                    CborValue::Text("Unknown".into()),
                    CborValue::Array(vec![CborValue::Array(vec![
                        CborValue::Text("GHC.Internal.Maybe".into()),
                        CborValue::Text("Just".into()),
                        CborValue::Text("Constructor".into()),
                    ])]),
                ])]),
            ]),
            CborValue::Array(vec![
                CborValue::Text("Ambiguous".into()),
                CborValue::Text("Result".into()),
                CborValue::Array(vec![]),
            ]),
            CborValue::Array(vec![
                CborValue::Text("NotFound".into()),
                CborValue::Text("nope".into()),
            ]),
            CborValue::Array(vec![
                CborValue::Text("Browse".into()),
                CborValue::Text("Tidepool.Actors.Exomonad".into()),
                CborValue::Bool(true),
                CborValue::Array(vec![]),
            ]),
        ]);
        let decoded = decode_inspections(&encoded(receipt)).unwrap();
        assert!(matches!(
            decoded[0],
            InspectionResult::Type {
                availability: InspectionAvailability::Unknown,
                ..
            }
        ));
        assert!(
            matches!(decoded[1], InspectionResult::Info { ref entries, .. }
            if entries[0].availability == InspectionAvailability::Unknown
                && entries[0].references == [IdentifierRef {
                    module: "GHC.Internal.Maybe".into(), name: "Just".into(),
                    namespace: IdentifierNamespace::Constructor,
                }])
        );
        assert_eq!(
            decoded[2],
            InspectionResult::Ambiguous {
                query: "Result".into(),
                entries: vec![],
            }
        );
        assert_eq!(
            decoded[3],
            InspectionResult::NotFound {
                query: "nope".into()
            }
        );
        assert!(matches!(
            decoded[4],
            InspectionResult::Browse { expanded: true, .. }
        ));
    }

    #[test]
    fn decodes_structured_type_and_typed_query_error() {
        let scope = CborValue::Array(vec![
            CborValue::Text("PublicModule".into()),
            CborValue::Text("Project.Work".into()),
        ]);
        let query = CborValue::Array(vec![
            scope.clone(),
            CborValue::Text("Value".into()),
            CborValue::Text("work".into()),
        ]);
        let identifier = CborValue::Array(vec![
            CborValue::Text("Project.Work".into()),
            CborValue::Text("work".into()),
            CborValue::Text("Value".into()),
        ]);
        let expression = CborValue::Array(vec![
            CborValue::Text("Eq a => a -> Bool".into()),
            CborValue::Array(vec![CborValue::Text("a".into())]),
            CborValue::Array(vec![CborValue::Text("Eq a".into())]),
        ]);
        let provenance = CborValue::Array(vec![
            scope,
            CborValue::Integer(7.into()),
            CborValue::Text("scope-abc".into()),
        ]);
        let receipt = complete_receipt(vec![
            CborValue::Array(vec![
                CborValue::Text("StructuredTypeOk".into()),
                CborValue::Array(vec![identifier, expression, provenance]),
            ]),
            CborValue::Array(vec![
                CborValue::Text("StructuredError".into()),
                CborValue::Array(vec![CborValue::Text("Unknown".into()), query]),
            ]),
        ]);

        let decoded = decode_inspections(&encoded(receipt)).unwrap();
        let InspectionResult::StructuredType(Ok(info)) = &decoded[0] else {
            panic!("expected structured type: {:?}", decoded[0]);
        };
        assert_eq!(info.identifier.module, "Project.Work");
        assert_eq!(info.expression.variables, ["a"]);
        assert_eq!(info.expression.constraints, ["Eq a"]);
        assert_eq!(info.provenance.generation, 7);
        assert!(matches!(
            &decoded[1],
            InspectionResult::StructuredInfo(Err(QueryError::Unknown(NameQuery {
                scope: NameScope::PublicModule(module),
                namespace: NameNamespace::Value,
                name,
            }))) if module == "Project.Work" && name == "work"
        ));
    }

    #[test]
    fn rejects_version_shape_and_unknown_tag() {
        for value in [
            CborValue::Array(vec![
                CborValue::Text("TPINSP005".into()),
                CborValue::Array(vec![]),
            ]),
            CborValue::Array(vec![
                CborValue::Text("TPINSP004".into()),
                CborValue::Array(vec![CborValue::Text("Other".into())]),
            ]),
            CborValue::Array(vec![CborValue::Text("TPINSP001".into())]),
        ] {
            assert!(decode_inspections(&encoded(value)).is_err());
        }

        let mut trailing = encoded(complete_receipt(vec![CborValue::Array(vec![
            CborValue::Text("NotFound".into()),
            CborValue::Text("x".into()),
        ])]));
        trailing.push(0);
        assert!(decode_inspections(&trailing).is_err());
    }

    #[test]
    fn decodes_row_availability_and_rejects_unknown_values() {
        let receipt = |availability: &str| {
            complete_receipt(vec![CborValue::Array(vec![
                CborValue::Text("TypeMatches".into()),
                CborValue::Text("Eff effects ()".into()),
                CborValue::Array(vec![CborValue::Array(vec![
                    CborValue::Text("run".into()),
                    CborValue::Null,
                    CborValue::Text("Eff effects ()".into()),
                    CborValue::Text("Usable".into()),
                    CborValue::Text(availability.into()),
                    CborValue::Array(vec![]),
                ])]),
            ])])
        };
        for (encoded_name, expected) in [
            ("Available", InspectionAvailability::Available),
            ("Polymorphic", InspectionAvailability::Polymorphic),
            ("Unavailable", InspectionAvailability::Unavailable),
            ("Unknown", InspectionAvailability::Unknown),
        ] {
            let decoded = decode_inspections(&encoded(receipt(encoded_name))).unwrap();
            assert!(
                matches!(&decoded[0], InspectionResult::TypeMatches { matches, .. }
                if matches[0].availability == expected)
            );
        }
        assert!(decode_inspections(&encoded(receipt("Guess"))).is_err());
    }

    #[test]
    fn selective_lookup_rich_work_does_not_grow_with_unrelated_exports() {
        eval_harness::require_extract();
        // Each counted case is process-isolated. Direct execution keeps the
        // compiler-owned work census visible through its ordinary stderr.
        std::env::remove_var(tidepool_extract_cmd::DAEMON_SOCKET_ENV);
        // The explicit frontend profile owns timing policy, including when
        // this case is selected by a campaign whose default is minimal.
        std::env::set_var(
            "TIDEPOOL_TEST_TRACE_PROFILE",
            tidepool_extract_cmd::CompilerTraceProfile::Full.name(),
        );
        let mut observed = Vec::new();
        let artifact_root = std::env::var_os("TIDEPOOL_TEST_ARTIFACT_ROOT");
        for unrelated in [0, 64] {
            let mut root = match &artifact_root {
                Some(parent) => tempfile::Builder::new()
                    .prefix(&format!("lookup-work-{unrelated}-"))
                    .tempdir_in(parent)
                    .unwrap(),
                None => tempfile::tempdir().unwrap(),
            };
            root.disable_cleanup(artifact_root.is_some());
            let mut module =
                String::from("module LookupWork where\nrequested :: Int\nrequested = 17\n");
            for index in 0..unrelated {
                module.push_str(&format!(
                    "unrelated{index} :: Eq a => a -> Bool\nunrelated{index} x = x == x\n"
                ));
            }
            std::fs::write(root.path().join("LookupWork.hs"), module).unwrap();
            let source = root.path().join("Expr.hs");
            std::fs::write(
                &source,
                "module Expr where\nimport qualified LookupWork as Work\n",
            )
            .unwrap();
            let output = root.path().join("lookup.cbor");
            let mut command = ExtractCmd::new().unwrap();
            command
                .output_dir(root.path())
                .inspect_out(&output)
                .includes(&[root.path()]);
            for query in ["Work.requested", "LookupWork.requested"] {
                command.input(&source).inspect_lookup_name(
                    tidepool_extract_cmd::LookupNameInspection {
                        query: query.into(),
                        qualifier_module: Some("LookupWork".into()),
                        mode: LookupNameMode::Name,
                    },
                );
            }
            command
                .input(&source)
                .inspect_exact_name(tidepool_extract_cmd::ExactNameInspection {
                    scope: tidepool_extract_cmd::InspectionScope::PublicModule("LookupWork".into()),
                    namespace: tidepool_extract_cmd::InspectionNamespace::Value,
                    name: "requested".into(),
                });
            let run = command.bind().unwrap().execute(&command).unwrap();
            std::fs::write(
                root.path().join("lookup.stderr"),
                run.stderr_lossy().as_bytes(),
            )
            .unwrap();
            assert!(run.success(), "{}", run.stderr_lossy());
            let results = decode_inspections(&std::fs::read(&output).unwrap()).unwrap();
            assert!(matches!(&results[..], [
                InspectionResult::Lookup { answer: LookupAnswer::Info(first), .. },
                InspectionResult::Lookup { answer: LookupAnswer::Info(second), .. },
                InspectionResult::Lookup { answer: LookupAnswer::Info(third), .. },
            ] if [first, second, third].iter().all(|entries| entries.len() == 1 && entries[0].name == "requested")));
            assert!(
                matches!(&results[0], InspectionResult::Lookup { scopes, .. } if scopes == &[NameScope::Current])
            );
            assert!(
                matches!(&results[1], InspectionResult::Lookup { scopes, .. } if scopes == &[NameScope::Current, NameScope::PublicModule("LookupWork".into())])
            );
            assert!(
                matches!(&results[2], InspectionResult::Lookup { scopes, .. } if scopes == &[NameScope::PublicModule("LookupWork".into())])
            );
            let stderr = run.stderr_lossy();
            let count = |name: &str| {
                let prefix = format!("tidepool-count name=inspection.{name} count=");
                let values = stderr
                    .lines()
                    .filter_map(|line| line.strip_prefix(&prefix))
                    .map(|value| {
                        value
                            .split_whitespace()
                            .next()
                            .unwrap()
                            .parse::<u64>()
                            .unwrap()
                    })
                    .collect::<Vec<_>>();
                assert_eq!(values.len(), 1, "missing or repeated work census: {stderr}");
                values[0]
            };
            let work = (
                count("rich_entries"),
                count("availability_checks"),
                count("resolved_names"),
                count("constraint_solves"),
            );
            assert_eq!(work.0, 3, "one rich projection per selected name: {stderr}");
            assert_eq!(
                work.3, 0,
                "the selected Int value requires no constraint solving"
            );
            println!("lookup work unrelated_exports={unrelated} rich_entries={} availability_checks={} resolved_names={} constraint_solves={}", work.0, work.1, work.2, work.3);
            observed.push(work);
            // A complete browse remains explicit and must expose the extra
            // entries. This control proves the census detects the old work.
            let mut browse = ExtractCmd::new().unwrap();
            let browse_output = root.path().join("browse.cbor");
            browse
                .output_dir(root.path())
                .inspect_out(&browse_output)
                .includes(&[root.path()])
                .input(&source)
                .inspect_browse("LookupWork", false);
            let run = browse.bind().unwrap().execute(&browse).unwrap();
            std::fs::write(
                root.path().join("browse.stderr"),
                run.stderr_lossy().as_bytes(),
            )
            .unwrap();
            assert!(run.success(), "{}", run.stderr_lossy());
            let results = decode_inspections(&std::fs::read(&browse_output).unwrap()).unwrap();
            assert!(
                matches!(&results[..], [InspectionResult::Browse { entries, .. }] if entries.len() == unrelated + 1)
            );
            assert!(run.stderr_lossy().contains(&format!(
                "tidepool-count name=inspection.rich_entries count={} ",
                unrelated + 1
            )));
            assert!(run.stderr_lossy().contains(&format!(
                "tidepool-count name=inspection.constraint_solves count={unrelated} "
            )));
            for line in run
                .stderr_lossy()
                .lines()
                .filter(|line| line.starts_with("tidepool-count name=inspection."))
            {
                println!("browse work unrelated_exports={unrelated} {line}");
            }
        }
        assert_eq!(
            observed[0], observed[1],
            "unrelated exports changed rich lookup work"
        );
    }

    #[test]
    fn qualified_info_and_browse_share_one_checked_target() {
        eval_harness::require_extract();
        // The assertion reads the worker's `tidepool-checked` accounting
        // lines, which a compile daemon filters from the stderr it returns.
        // Bind a direct worker so the evidence is visible on every route.
        // Nextest runs each test in its own process, so this is not shared.
        std::env::remove_var(tidepool_extract_cmd::DAEMON_SOCKET_ENV);
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("Expr.hs");
        std::fs::write(
            &source,
            "module Expr where\nimport qualified Data.Maybe as M\n",
        )
        .unwrap();
        let output = temp.path().join("inspection.cbor");
        let mut command = ExtractCmd::new().unwrap();
        command
            .output_dir(temp.path())
            .inspect_out(&output)
            .input(&source)
            .inspect_info("M.Maybe")
            .input(&source)
            .inspect_browse("Data.Maybe", false);
        let run = command.bind().unwrap().execute(&command).unwrap();
        assert!(run.success(), "{}", run.stderr_lossy());
        let stderr = run.stderr_lossy();
        assert_eq!(
            stderr
                .matches("tidepool-checked module=Expr target=True")
                .count(),
            1,
            "{stderr}"
        );
        assert!(
            !stderr.contains("tidepool-target phase=desugar"),
            "metadata entered executable compilation: {stderr}"
        );
        let results = decode_inspections(&std::fs::read(output).unwrap()).unwrap();
        assert!(matches!(
            results.as_slice(),
            [
                InspectionResult::Info { .. },
                InspectionResult::Browse { .. }
            ]
        ));
    }

    #[test]
    fn one_inspection_compile_answers_type_info_and_browse_queries() {
        eval_harness::require_extract();
        let include = tempfile::tempdir().unwrap();
        let session = tempfile::tempdir().unwrap();
        std::fs::write(
            include.path().join("BrowseFixture.hs"),
            concat!(
                "{-# LANGUAGE TypeFamilies, DuplicateRecordFields #-}\n",
                "module BrowseFixture (Public(..), Record(..), TwinA(..), TwinB(..), Abstract, Opaque, Solo(..), Alias, Family, Service(..), constrained, exportedValue, Maybe(..)) where\n",
                "import Prelude\n",
                "import Data.Maybe (Maybe(..))\n",
                "data Public = First | Second\n",
                "data Record = Record { recordField :: Int }\n",
                "data TwinA = TwinA { sharedField :: Int }\n",
                "data TwinB = TwinB { sharedField :: Bool }\n",
                "data Abstract = Hidden\n",
                "newtype Opaque = Opaque Int\n",
                "data Solo = Solo\n",
                "type Alias a = Maybe a\n",
                "type family Family a\n",
                "class Service a where service :: a -> Int\n",
                "constrained :: Eq a => a -> Bool\n",
                "constrained x = x == x\n",
                "exportedValue :: Int\n",
                "exportedValue = 42\n",
            ),
        )
        .unwrap();
        let preamble = concat!(
            "{-# LANGUAGE NoImplicitPrelude, NoMonomorphismRestriction, PartialTypeSignatures #-}\n",
            "module Expr where\n",
            "import BrowseFixture\n",
            "import qualified BrowseFixture as Alias\n",
            "localPreambleValue = exportedValue\n",
            "__tidepool_fixture = exportedValue\n",
        );
        let current = ScopeProvenance {
            scope: NameScope::Current,
            generation: 11,
            fingerprint: "current-scope".into(),
        };
        let public = ScopeProvenance {
            scope: NameScope::PublicModule("BrowseFixture".into()),
            generation: 11,
            fingerprint: "public-module".into(),
        };
        let current_query = |namespace, name: &str| NameQuery {
            scope: NameScope::Current,
            namespace,
            name: name.into(),
        };
        let public_query = |namespace, name: &str| NameQuery {
            scope: NameScope::PublicModule("BrowseFixture".into()),
            namespace,
            name: name.into(),
        };
        let mut queries = vec![
            InspectionQuery::TypeOf("exportedValue".into()),
            InspectionQuery::Info("Public".into()),
            InspectionQuery::TypeOf("missing + 1".into()),
            InspectionQuery::Browse {
                module: "No.Such.Module".into(),
                expanded: false,
            },
            InspectionQuery::Browse {
                module: "BrowseFixture".into(),
                expanded: false,
            },
            InspectionQuery::Browse {
                module: "BrowseFixture".into(),
                expanded: true,
            },
            InspectionQuery::StructuredType {
                query: current_query(NameNamespace::Value, "constrained"),
                provenance: current.clone(),
            },
            InspectionQuery::StructuredInfo {
                query: public_query(NameNamespace::Type, "Record"),
                provenance: public.clone(),
            },
            InspectionQuery::StructuredInfo {
                query: public_query(NameNamespace::Type, "Abstract"),
                provenance: public.clone(),
            },
            InspectionQuery::StructuredInfo {
                query: public_query(NameNamespace::Type, "Alias"),
                provenance: public.clone(),
            },
            InspectionQuery::StructuredInfo {
                query: current_query(NameNamespace::Any, "Solo"),
                provenance: current.clone(),
            },
            InspectionQuery::StructuredInfo {
                query: current_query(NameNamespace::Constructor, "Solo"),
                provenance: current.clone(),
            },
            InspectionQuery::StructuredInfo {
                query: current_query(NameNamespace::Value, "missingName"),
                provenance: current.clone(),
            },
            InspectionQuery::StructuredInfo {
                query: NameQuery {
                    scope: NameScope::PublicModule("No.Such.Module".into()),
                    namespace: NameNamespace::Type,
                    name: "Missing".into(),
                },
                provenance: ScopeProvenance {
                    scope: NameScope::PublicModule("No.Such.Module".into()),
                    generation: 11,
                    fingerprint: "missing-module".into(),
                },
            },
            InspectionQuery::StructuredInfo {
                query: public_query(NameNamespace::Type, "Family"),
                provenance: public.clone(),
            },
            InspectionQuery::StructuredInfo {
                query: public_query(NameNamespace::Type, "Opaque"),
                provenance: public,
            },
        ];
        queries.push(InspectionQuery::Info("Alias.Public".into()));
        queries.push(InspectionQuery::Info("Alias.exportedValue".into()));
        queries.push(InspectionQuery::Info("Missing.Public".into()));
        queries.push(InspectionQuery::TypeSearch("Public".into()));
        queries.push(InspectionQuery::TypeSearch("Public ->".into()));
        queries.push(InspectionQuery::TypeSearch("Public -> _".into()));
        queries.push(InspectionQuery::ScopeBrowse);
        // A preamble declaration lives only in the typechecked query module;
        // a request's `respond` is bound this way.
        queries.push(InspectionQuery::Info("localPreambleValue".into()));
        let exact_start = queries.len();
        for (namespace, name) in [
            (IdentifierNamespace::Type, "Abstract"),
            (IdentifierNamespace::Constructor, "Hidden"),
            (IdentifierNamespace::Field, "recordField"),
            (IdentifierNamespace::Value, "recordField"),
            (IdentifierNamespace::Type, "Solo"),
            (IdentifierNamespace::Constructor, "Solo"),
            (IdentifierNamespace::Constructor, "Just"),
        ] {
            queries.push(InspectionQuery::ExactName(ExactNameQuery {
                scope: NameScope::PublicModule("BrowseFixture".into()),
                namespace,
                name: name.into(),
            }));
        }
        queries.push(InspectionQuery::ExactName(ExactNameQuery {
            scope: NameScope::PublicModule("Tidepool.Session.Val.G1".into()),
            namespace: IdentifierNamespace::Value,
            name: "hidden".into(),
        }));
        queries.push(InspectionQuery::ExactName(ExactNameQuery {
            scope: NameScope::Current,
            namespace: IdentifierNamespace::Value,
            name: "__tidepool_fixture".into(),
        }));
        queries.push(InspectionQuery::LookupName {
            query: "Alias.exportedValu".into(),
            qualifier_module: Some("BrowseFixture".into()),
            mode: LookupNameMode::Name,
        });
        for namespace in [IdentifierNamespace::Field, IdentifierNamespace::Value] {
            queries.push(InspectionQuery::ExactName(ExactNameQuery {
                scope: NameScope::Current,
                namespace,
                name: "recordField".into(),
            }));
        }
        let differential_start = queries.len();
        for scope in [
            NameScope::Current,
            NameScope::PublicModule("BrowseFixture".into()),
        ] {
            for name in [
                "exportedValue",
                "recordField",
                "sharedField",
                "Service",
                "service",
                "Abstract",
                "Hidden",
                "Solo",
                "Just",
                "Alias",
                "Family",
            ] {
                for namespace in [
                    IdentifierNamespace::Value,
                    IdentifierNamespace::Field,
                    IdentifierNamespace::Type,
                    IdentifierNamespace::Constructor,
                ] {
                    queries.push(InspectionQuery::ExactName(ExactNameQuery {
                        scope: scope.clone(),
                        namespace,
                        name: name.into(),
                    }));
                }
            }
        }
        let results = tidepool_testing::with_settlement(|settlement| {
            run_inspections(
                InspectionRequest {
                    exact_context: None,
                    preamble,
                    imports: "",
                    include: &[include.path()],
                    session_root: session.path(),
                    inject_modules: &[],
                    queries: &queries,
                    effects: None,
                },
                settlement,
            )
        })
        .unwrap();

        assert_eq!(results.len(), queries.len());
        let selected = &results[exact_start..];
        assert!(
            matches!(&selected[0], InspectionResult::Lookup { answer: LookupAnswer::Info(entries), .. }
            if entries.len() == 1 && entries[0].kind == "type" && !entries[0].display.contains("Hidden"))
        );
        for index in [1, 3] {
            assert!(matches!(
                &selected[index],
                InspectionResult::Lookup {
                    answer: LookupAnswer::Missing { .. },
                    ..
                }
            ));
        }
        for (index, kind) in [
            (2, "record-selector"),
            (4, "type"),
            (5, "constructor"),
            (6, "constructor"),
        ] {
            assert!(
                matches!(&selected[index], InspectionResult::Lookup { answer: LookupAnswer::Info(entries), .. }
                if entries.len() == 1 && entries[0].kind == kind),
                "{:?}",
                selected[index]
            );
        }
        assert!(
            matches!(&selected[6], InspectionResult::Lookup { answer: LookupAnswer::Info(entries), .. }
            if entries[0].module.as_deref().is_some_and(|owner| owner != "BrowseFixture"))
        );
        assert!(
            matches!(&selected[7], InspectionResult::Lookup { scopes, answer: LookupAnswer::ModuleNotFound(_), .. }
            if scopes.is_empty()),
            "protected scope refusal claimed a GHC search: {:?}",
            selected[7]
        );

        assert!(
            matches!(
                &selected[8],
                InspectionResult::Lookup {
                    answer: LookupAnswer::Missing { .. },
                    ..
                }
            ),
            "private generated reference escaped current-scope browse visibility: {:?}",
            selected[8]
        );
        assert!(
            matches!(&selected[9], InspectionResult::Lookup { answer: LookupAnswer::Missing { export_names }, .. }
            if export_names.iter().any(|name| name == "exportedValue")
                && ["Hidden", "recordField", "Just"].iter().all(|hidden| !export_names.iter().any(|name| name == hidden))),
            "cheap suggestions changed compact export visibility: {:?}",
            selected[9]
        );
        assert!(
            matches!(&selected[10], InspectionResult::Lookup { answer: LookupAnswer::Info(entries), .. }
            if entries.len() == 1 && entries[0].kind == "record-selector"),
            "current field lookup disagrees with its exported selector: {:?}",
            selected[10]
        );
        assert!(matches!(
            &selected[11],
            InspectionResult::Lookup {
                answer: LookupAnswer::Missing { .. },
                ..
            }
        ));
        // Expanded browse is the independent old reference-selection oracle.
        // Compare complete entries, including two distinct same-spelled fields.
        for (query, observed) in queries[differential_start..]
            .iter()
            .zip(&results[differential_start..])
        {
            let InspectionQuery::ExactName(query) = query else {
                unreachable!()
            };
            let InspectionResult::Browse { entries, .. } = &results[match query.scope {
                NameScope::Current => 22,
                NameScope::PublicModule(_) => 5,
            }] else {
                panic!("missing expanded browse oracle")
            };
            let namespace = |entry: &&InfoEntry| match entry.kind.as_str() {
                "type" => IdentifierNamespace::Type,
                "constructor" => IdentifierNamespace::Constructor,
                "record-selector" => IdentifierNamespace::Field,
                _ => IdentifierNamespace::Value,
            };
            let mut expected = entries
                .iter()
                .filter(|entry| entry.name == query.name && namespace(entry) == query.namespace)
                .cloned()
                .collect::<Vec<_>>();
            if query.name == "sharedField" && query.namespace == IdentifierNamespace::Field {
                assert_eq!(expected.len(), 2, "duplicate-field oracle lost an owner");
            }
            let InspectionResult::Lookup { answer, .. } = observed else {
                panic!("missing exact answer: {observed:?}")
            };
            let mut actual = match (expected.len(), answer) {
                (0, LookupAnswer::Missing { export_names }) if export_names.is_empty() => vec![],
                (1, LookupAnswer::Info(entries)) => entries.clone(),
                (count, LookupAnswer::Ambiguous(entries)) if count > 1 => entries.clone(),
                _ => panic!("wrong exact outcome for {query:?}: expected {expected:?}, observed {observed:?}"),
            };
            expected.sort_by_cached_key(|entry| format!("{entry:?}"));
            actual.sort_by_cached_key(|entry| format!("{entry:?}"));
            assert_eq!(
                actual, expected,
                "exact query differs from expanded browse: {query:?}"
            );
        }
        let InspectionResult::Browse {
            module, entries, ..
        } = &results[22]
        else {
            panic!("expected current-scope browse, got {:?}", results[22]);
        };
        assert!(module.is_empty());
        assert!(entries.iter().any(|entry| entry.name == "exportedValue"));
        assert!(entries.iter().any(|entry| entry.name == "recordField"));
        assert!(!entries.iter().any(|entry| entry.name == "Hidden"));
        assert!(!entries
            .iter()
            .any(|entry| entry.name.starts_with("__tidepool_")));

        assert!(results[0].render().contains("exportedValue :: Int"));
        assert!(results[1].render().contains("data Public"));
        assert!(
            results[23].render().contains("localPreambleValue :: Int"),
            "{:?}",
            results[23]
        );
        assert!(matches!(results[2], InspectionResult::Rejected { .. }));
        assert_eq!(
            results[3],
            InspectionResult::ModuleNotFound {
                module: "No.Such.Module".into(),
                scopes: vec![NameScope::PublicModule("No.Such.Module".into())],
            }
        );
        // Alias.Public / Alias.exportedValue / Missing.Public / the three TypeSearch
        // queries were originally the last six entries pushed onto `queries` and were
        // checked at indices 6-11. The StructuredType/StructuredInfo queries were later
        // spliced into the `vec![...]` literal ahead of those pushes, shifting every
        // pushed query down by ten slots (they now live at indices 16-21) without these
        // assertions being renumbered. Left at 6-11 they instead re-checked the spliced-in
        // StructuredType/StructuredInfo queries, contradicting the dedicated structured
        // assertions for those indices below and failing outright on results[6].
        assert!(results[16].render().contains("data Public"));
        assert!(results[17].render().contains("exportedValue :: Int"));
        assert!(matches!(results[18], InspectionResult::NotFound { .. }));
        assert!(
            matches!(results[19], InspectionResult::TypeMatches { .. }),
            "{:?}",
            results[19]
        );
        assert!(matches!(results[20], InspectionResult::Rejected { .. }));
        assert!(
            matches!(results[21], InspectionResult::TypeMatches { .. }),
            "{:?}",
            results[21]
        );
        let grouped = results[4].render();
        assert!(grouped.starts_with("-- BrowseFixture\n"));
        assert!(grouped.contains("data Public"));
        assert!(grouped.contains("class Service"));
        assert!(grouped.contains("exportedValue :: Int"));
        assert!(!grouped.lines().any(|line| line.starts_with("First ::")));

        // A browse must not advertise a constructor the module keeps to itself.
        // `Abstract` and `Opaque` are exported without `(..)`, so a cell cannot
        // write `Hidden`; showing it sent a live agent into four rounds of
        // recovery after `Data constructor not in scope`.
        assert!(!grouped.contains("Hidden"), "{grouped}");
        assert!(grouped.contains("data Abstract"), "{grouped}");
        // The type `Opaque` is exported and its like-named data constructor is
        // not; they differ by namespace and must be treated separately.
        assert!(
            !grouped.lines().any(|line| line.contains("newtype Opaque")
                && line.contains('=')
                && !line.contains("...")),
            "{grouped}"
        );
        // A constructor the module does export still renders in place.
        assert!(grouped.contains("First"), "{grouped}");
        let InspectionResult::Browse { entries, .. } = &results[5] else {
            panic!("expected expanded browse");
        };
        let references = |name: &str| {
            &entries
                .iter()
                .find(|entry| entry.name == name)
                .unwrap()
                .references
        };
        assert!(references("constrained")
            .iter()
            .any(|reference| reference.name == "Eq"));
        assert!(references("Alias")
            .iter()
            .any(|reference| reference.name == "Maybe"));
        assert!(references("recordField")
            .iter()
            .any(|reference| reference.name == "Record"
                && reference.namespace == IdentifierNamespace::Type));
        assert!(references("Record")
            .iter()
            .any(|reference| reference.name == "recordField"
                && reference.namespace == IdentifierNamespace::Field));
        assert!(references("Abstract").is_empty());
        assert!(references("Opaque").is_empty());
        assert!(references("First")
            .iter()
            .any(|reference| reference.name == "Public"));
        let expanded = results[5].render();
        assert!(!expanded.contains("Hidden"), "{expanded}");
        assert!(
            !expanded.lines().any(|line| line.contains("newtype Opaque")
                && line.contains('=')
                && !line.contains("...")),
            "{expanded}"
        );
        assert!(expanded.contains("First :: Public"), "{expanded}");
        assert!(expanded.contains("service ::"), "{expanded}");
        assert!(expanded.contains("data Maybe"), "{expanded}");

        let InspectionResult::StructuredType(Ok(constrained)) = &results[6] else {
            panic!("expected constrained function type: {:?}", results[6]);
        };
        assert_eq!(constrained.identifier.name, "constrained");
        assert!(
            constrained
                .expression
                .constraints
                .iter()
                .any(|constraint| constraint.contains("Eq")),
            "{:?}",
            constrained.expression
        );
        assert_eq!(constrained.provenance.fingerprint, "current-scope");

        let InspectionResult::StructuredInfo(Ok(record)) = &results[7] else {
            panic!("expected record declaration: {:?}", results[7]);
        };
        let DeclarationInfo::Data { constructors, .. } = &record.declaration else {
            panic!("expected data declaration: {:?}", record.declaration);
        };
        assert_eq!(constructors.len(), 1);
        assert_eq!(constructors[0].fields[0].name, "recordField");

        let InspectionResult::StructuredInfo(Ok(abstract_type)) = &results[8] else {
            panic!("expected abstract type declaration: {:?}", results[8]);
        };
        assert!(matches!(
            &abstract_type.declaration,
            DeclarationInfo::Data { constructors, .. } if constructors.is_empty()
        ));

        let InspectionResult::StructuredInfo(Ok(alias)) = &results[9] else {
            panic!("expected type synonym: {:?}", results[9]);
        };
        assert!(
            matches!(
                &alias.declaration,
                DeclarationInfo::TypeSynonym { parameters, body }
                    if parameters.len() == 1 && body.canonical.contains("Maybe")
            ),
            "{:?}",
            alias.declaration
        );
        assert!(matches!(
            &results[10],
            InspectionResult::StructuredInfo(Err(QueryError::Ambiguous(query, candidates)))
                if query.name == "Solo" && candidates.len() == 2
        ));

        let InspectionResult::StructuredInfo(Ok(constructor)) = &results[11] else {
            panic!("expected constructor declaration: {:?}", results[11]);
        };
        assert!(matches!(
            (&constructor.declaration, &constructor.parent),
            (DeclarationInfo::Constructor { parent, .. }, Some(recorded_parent))
                if parent.name == "Solo" && recorded_parent == parent
        ));
        assert!(matches!(
            &results[12],
            InspectionResult::StructuredInfo(Err(QueryError::Unknown(query)))
                if query.name == "missingName"
        ));
        assert!(matches!(
            &results[13],
            InspectionResult::StructuredInfo(Err(QueryError::UnknownModule(module)))
                if module == "No.Such.Module"
        ));
        assert!(matches!(
            &results[14],
            InspectionResult::StructuredInfo(Err(QueryError::Unsupported(detail)))
                if detail.contains("unsupported")
        ));
        assert!(matches!(
            &results[15],
            InspectionResult::StructuredInfo(Err(QueryError::Unsupported(detail)))
                if detail.contains("abstract newtype constructor")
        ));
    }

    #[test]
    fn info_lookup_with_effect_row_imports_its_generated_qualified_names() {
        eval_harness::require_extract();
        let session = tempfile::tempdir().unwrap();
        let mut effects_include = eval_harness::effects_include().to_vec();
        effects_include.push(eval_harness::prelude_path());
        effects_include.push(eval_harness::actor_sources_path());
        let include = effects_include
            .iter()
            .map(std::path::PathBuf::as_path)
            .collect::<Vec<_>>();
        let inspect = |queries: &[InspectionQuery]| {
            tidepool_testing::with_settlement(|settlement| {
                run_inspections(
                    InspectionRequest {
                        exact_context: None,
                        preamble: concat!(
                            "{-# LANGUAGE NoImplicitPrelude, DataKinds, TypeOperators #-}\n",
                            "{-# LANGUAGE FlexibleContexts, TypeFamilies, ConstraintKinds #-}\n",
                            "module Expr where\n",
                            "import Prelude\n",
                            "import qualified Tidepool.Effects.Core as LookupEffects\n",
                            "import Tidepool.Actors.Spawn (checkpoint, spawnSubagent)\n",
                        ),
                        imports: "",
                        include: &include,
                        session_root: session.path(),
                        inject_modules: &[],
                        queries,
                        effects: Some(&super::super::HaskellTypeSource::from(
                            "Tidepool.Effects.Core.AgentTools ': \
                 Tidepool.Agent.Contract.SyncEffects \
                 (Tidepool.Effects.Core.AgentLaunch ': '[])",
                        )),
                    },
                    settlement,
                )
            })
            .unwrap()
        };
        let names = [
            ("map", None),
            ("checkpoint", Some("ContextCheckpoint")),
            ("spawnSubagent", Some("AgentRef")),
        ];
        let queries = names.map(|(name, _)| InspectionQuery::Info(name.into()));
        let results = inspect(&queries);

        assert_eq!(results.len(), names.len());
        for (result, (name, result_type)) in results.iter().zip(names) {
            assert!(
                matches!(result, InspectionResult::Info { query, entries }
                    if query == name && entries.iter().any(|entry|
                        entry.name == name && entry.kind == "value"
                        && matches!(entry.availability,
                            InspectionAvailability::Available | InspectionAvailability::Polymorphic)
                        && result_type.is_none_or(|result_type|
                            entry.references.iter().any(|reference| reference.name == result_type)))),
                "{result:?}"
            );
        }

        // The homogeneous type-query path assembles an additional shared module.
        let queries =
            ["checkpoint", "spawnSubagent"].map(|name| InspectionQuery::TypeOf(name.into()));
        let results = inspect(&queries);
        assert_eq!(results.len(), 2);
        for (result, (name, result_type)) in results.iter().zip([
            ("checkpoint", "ContextCheckpoint"),
            ("spawnSubagent", "AgentRef"),
        ]) {
            assert!(
                matches!(result, InspectionResult::Type { expression, display, .. }
                    if expression == name && display.contains(result_type)),
                "{result:?}"
            );
        }
    }

    #[test]
    fn type_probe_batch_preserves_independent_types_and_rejected_siblings() {
        eval_harness::require_extract();
        let session = tempfile::tempdir().unwrap();
        let preamble = concat!(
            "{-# LANGUAGE NoImplicitPrelude, ExtendedDefaultRules #-}\n",
            "module Expr where\n",
            "import Prelude\n",
        );
        let valid_queries = [
            InspectionQuery::TypeOf("id".into()),
            InspectionQuery::TypeOf("1".into()),
            InspectionQuery::TypeOf("const".into()),
        ];
        let inspect = |queries: &[InspectionQuery]| {
            tidepool_testing::with_settlement(|settlement| {
                run_inspections(
                    InspectionRequest {
                        exact_context: None,
                        preamble,
                        imports: "",
                        include: &[],
                        session_root: session.path(),
                        inject_modules: &[],
                        queries,
                        effects: None,
                    },
                    settlement,
                )
            })
            .unwrap()
        };

        let valid = inspect(&valid_queries);
        let singleton_results = valid_queries
            .iter()
            .map(|query| inspect(std::slice::from_ref(query)).remove(0))
            .collect::<Vec<_>>();
        assert_eq!(valid, singleton_results);
        assert!(matches!(
            &valid[0],
            InspectionResult::Type { expression, display, .. }
                if expression == "id" && display == "a -> a"
        ));
        assert!(matches!(
            &valid[1],
            InspectionResult::Type { expression, display, .. }
                if expression == "1" && !display.is_empty()
        ));
        assert!(matches!(
            &valid[2],
            InspectionResult::Type { expression, display, .. }
                if expression == "const" && display == "a -> b -> a"
        ));

        let invalid_queries = [
            InspectionQuery::TypeOf("id".into()),
            InspectionQuery::TypeOf("missing + 1".into()),
            InspectionQuery::TypeOf("const".into()),
        ];
        let isolated = inspect(&invalid_queries);
        assert!(
            matches!(&isolated[0], InspectionResult::Type { expression, .. } if expression == "id")
        );
        assert!(matches!(&isolated[1], InspectionResult::Rejected { .. }));
        assert!(
            matches!(&isolated[2], InspectionResult::Type { expression, .. } if expression == "const")
        );

        // Generated probe bindings share a module only in the batch form. If
        // authored source names that private prefix, keep singleton scoping so
        // one query cannot resolve another query's generated binder.
        let generated_name_queries = [
            InspectionQuery::TypeOf("id".into()),
            InspectionQuery::TypeOf("__tidepool_inspect_0 1".into()),
        ];
        let generated_name_results = inspect(&generated_name_queries);
        let generated_name_singletons = generated_name_queries
            .iter()
            .map(|query| inspect(std::slice::from_ref(query)).remove(0))
            .collect::<Vec<_>>();
        assert!(matches!(
            (&generated_name_results[0], &generated_name_singletons[0]),
            (InspectionResult::Type { display: batched, .. }, InspectionResult::Type { display: singleton, .. })
                if batched == singleton
        ));
        assert!(matches!(
            (&generated_name_results[1], &generated_name_singletons[1]),
            (
                InspectionResult::Rejected { .. },
                InspectionResult::Rejected { .. }
            )
        ));
    }

    #[test]
    fn checked_target_keeps_its_boot_interface_for_source_import_cycles() {
        eval_harness::require_extract();
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("Expr.hs-boot"),
            "module Expr where\nimport Prelude\neven' :: Int -> Bool\n",
        )
        .unwrap();
        std::fs::write(
            temp.path().join("Odd.hs"),
            concat!(
                "module Odd where\n",
                "import Prelude\n",
                "import {-# SOURCE #-} qualified Expr\n",
                "odd' :: Int -> Bool\n",
                "odd' n = n /= 0 && Expr.even' (n - 1)\n",
            ),
        )
        .unwrap();
        let source = temp.path().join("Expr.hs");
        std::fs::write(
            &source,
            assemble_inspection_module(
                concat!(
                    "module Expr where\n",
                    "import Prelude\n",
                    "import qualified Odd\n",
                    "even' :: Int -> Bool\n",
                    "even' n = n == 0 || Odd.odd' (n - 1)\n",
                ),
                "",
                &["even'".into()],
            ),
        )
        .unwrap();
        let output = temp.path().join("inspection.cbor");
        let mut command = ExtractCmd::new().unwrap();
        command
            .output_dir(temp.path())
            .inspect_out(&output)
            .includes([temp.path()])
            .session_root(temp.path())
            .input(&source)
            .inspect_type("even'");
        let run = command.bind().unwrap().execute(&command).unwrap();
        assert!(run.success(), "{}", run.stderr_lossy());
        let results = decode_inspections(&std::fs::read(output).unwrap()).unwrap();

        assert!(
            matches!(
                results.as_slice(),
                [InspectionResult::Type { display, .. }] if display == "Int -> Bool"
            ),
            "{results:?}"
        );
    }

    #[test]
    fn ninety_five_type_probes_share_one_valid_batch() {
        eval_harness::require_extract();
        let session = tempfile::tempdir().unwrap();
        let queries = (0..95)
            .map(|_| InspectionQuery::TypeOf("id".into()))
            .collect::<Vec<_>>();
        let results = tidepool_testing::with_settlement(|settlement| {
            run_inspections(
                InspectionRequest {
                    exact_context: None,
                    preamble: concat!(
                        "{-# LANGUAGE NoImplicitPrelude #-}\n",
                        "module Expr where\n",
                        "import Prelude\n",
                    ),
                    imports: "",
                    include: &[],
                    session_root: session.path(),
                    inject_modules: &[],
                    queries: &queries,
                    effects: None,
                },
                settlement,
            )
        })
        .unwrap();

        assert_eq!(results.len(), 95);
        assert!(results.iter().all(|result| matches!(
            result,
            InspectionResult::Type {
                expression,
                display,
                ..
            } if expression == "id" && display.contains("->")
        )));
    }
}
