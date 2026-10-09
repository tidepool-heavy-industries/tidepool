//! Structured lookup mechanics shared by the self-hosted lookup tool.
use crate::lookup_tool;
use tidepool_bridge_derive::{FromHaskell, ToHaskell};
use tidepool_runtime::session::{InspectionQuery, InspectionResult};

#[derive(Debug)]
pub(crate) enum LookupInspectionError {
    Compiler(tidepool_runtime::CompileError),
    Message(String),
}

impl From<String> for LookupInspectionError {
    fn from(message: String) -> Self {
        Self::Message(message)
    }
}

impl From<&str> for LookupInspectionError {
    fn from(message: &str) -> Self {
        Self::Message(message.into())
    }
}

fn render_lookup_inspection_error(error: &LookupInspectionError) -> String {
    match error {
        LookupInspectionError::Compiler(tidepool_runtime::CompileError::InputRejected(diags)) => {
            render_lookup_diagnostics("lookup compiler input rejected", diags)
        }
        LookupInspectionError::Compiler(tidepool_runtime::CompileError::Diagnostics(diags)) => {
            render_lookup_diagnostics("lookup Haskell query failed", diags)
        }
        LookupInspectionError::Compiler(tidepool_runtime::CompileError::WorkerFailure(diags)) => {
            render_lookup_diagnostics("lookup compiler worker failed", diags)
        }
        LookupInspectionError::Compiler(error) => error.to_string(),
        LookupInspectionError::Message(message) => message.clone(),
    }
}

fn render_lookup_diagnostics(
    heading: &str,
    diagnostics: &[tidepool_toolchain::diag::ExtractDiag],
) -> String {
    let details = diagnostics
        .iter()
        .map(|diagnostic| match &diagnostic.span {
            Some(span) if lookup_tool::is_generated_query_file(&span.file) => {
                format!("{}: {}", diagnostic.severity, diagnostic.message)
            }
            Some(span) => format!(
                "{}:{}:{}-{}:{}: {}: {}",
                span.file,
                span.start_line,
                span.start_col,
                span.end_line,
                span.end_col,
                diagnostic.severity,
                diagnostic.message
            ),
            None => format!("{}: {}", diagnostic.severity, diagnostic.message),
        })
        .collect::<Vec<_>>()
        .join("\n");
    let rendered = if details.is_empty() {
        heading.to_owned()
    } else {
        format!("{heading}:\n{details}")
    };
    lookup_tool::strip_generated_query_locations(&rendered)
}

#[derive(FromHaskell)]
#[haskell(name = "LookupRequest")]
pub(crate) struct LookupRequest {
    pub queries: Vec<String>,
    pub discover: bool,
    pub expected_view: Option<String>,
    pub candidate_limit: i64,
    pub references: Vec<LookupReference>,
}
#[derive(ToHaskell)]
#[haskell(module = "Tidepool.Effects.Core", name = "LookupBatch")]
pub(crate) struct LookupBatch {
    pub results: Vec<LookupResult>,
    pub candidates: Vec<LookupCandidate>,
    pub view: String,
    pub issue: Option<String>,
}
#[derive(ToHaskell)]
#[haskell(module = "Tidepool.Effects.Core", name = "LookupResult")]
pub(crate) struct LookupResult {
    pub query: String,
    pub outcome: LookupOutcome,
}
#[derive(ToHaskell)]
pub(crate) enum LookupOutcome {
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupFound")]
    Found(Vec<LookupEntry>, bool),
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupMissing")]
    Missing(Vec<String>, Vec<String>),
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupAmbiguous")]
    Ambiguous(Vec<LookupEntry>, bool),
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupRejected")]
    Rejected(String),
}
#[derive(ToHaskell)]
#[haskell(module = "Tidepool.Effects.Core", name = "LookupCandidate")]
pub(crate) struct LookupCandidate {
    pub query: String,
    pub origins: Vec<String>,
    pub summary: String,
    pub local: bool,
    pub reference: Option<LookupReference>,
}
#[derive(Clone, Debug, PartialEq, Eq, FromHaskell, ToHaskell)]
#[haskell(module = "Tidepool.Effects.Core", name = "LookupReference")]
pub(crate) struct LookupReference {
    module: String,
    name: String,
    namespace: LookupNamespace,
}
#[derive(Clone, Debug, PartialEq, Eq, FromHaskell, ToHaskell)]
pub(crate) enum LookupNamespace {
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupValueNamespace")]
    Value,
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupTypeNamespace")]
    Type,
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupConstructorNamespace")]
    Constructor,
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupFieldNamespace")]
    Field,
}
impl LookupNamespace {
    fn from_kind(kind: &str) -> Self {
        match kind {
            "type" => Self::Type,
            "constructor" => Self::Constructor,
            "record-selector" => Self::Field,
            _ => Self::Value,
        }
    }
}
impl From<&tidepool_runtime::session::IdentifierRef> for LookupReference {
    fn from(r: &tidepool_runtime::session::IdentifierRef) -> Self {
        use tidepool_runtime::session::IdentifierNamespace as N;
        Self {
            module: r.module.clone(),
            name: r.name.clone(),
            namespace: match r.namespace {
                N::Type => LookupNamespace::Type,
                N::Constructor => LookupNamespace::Constructor,
                N::Field => LookupNamespace::Field,
                N::Value => LookupNamespace::Value,
            },
        }
    }
}
#[derive(ToHaskell)]
#[haskell(module = "Tidepool.Effects.Core", name = "LookupEntry")]
pub(crate) struct LookupEntry {
    name: String,
    module: Option<String>,
    declaration: String,
    kind: LookupKind,
    availability: LookupAvailability,
    origin: LookupOrigin,
    quality: LookupQuality,
    usage: Option<String>,
    example: Option<crate::usage_pointer::LookupExample>,
}
#[derive(ToHaskell)]
enum LookupKind {
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupValue")]
    Value,
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupClassMethod")]
    ClassMethod,
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupRecordSelector")]
    RecordSelector,
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupConstructor")]
    Constructor,
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupType")]
    Type,
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupCoercion")]
    Coercion,
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupDocumentation")]
    Documentation,
}
#[derive(ToHaskell)]
enum LookupAvailability {
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupAvailable")]
    Available,
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupPolymorphic")]
    Polymorphic,
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupUnknown")]
    Unknown,
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupUnavailable")]
    Unavailable,
}
#[derive(ToHaskell)]
enum LookupOrigin {
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupModuleExport")]
    ModuleExport,
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupLiveBinding")]
    LiveBinding,
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupDocumentationOrigin")]
    DocumentationOrigin,
}
#[derive(ToHaskell)]
enum LookupQuality {
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupExact")]
    Exact,
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupUsable")]
    Usable,
}
impl From<lookup_tool::LookupEntry> for LookupEntry {
    fn from(e: lookup_tool::LookupEntry) -> Self {
        Self {
            name: e.name,
            module: e.defining_module,
            declaration: e.signature_or_declaration,
            kind: match e.kind {
                lookup_tool::LookupEntryKind::Value => LookupKind::Value,
                lookup_tool::LookupEntryKind::ClassMethod => LookupKind::ClassMethod,
                lookup_tool::LookupEntryKind::RecordSelector => LookupKind::RecordSelector,
                lookup_tool::LookupEntryKind::Constructor => LookupKind::Constructor,
                lookup_tool::LookupEntryKind::Type => LookupKind::Type,
                lookup_tool::LookupEntryKind::Coercion => LookupKind::Coercion,
                lookup_tool::LookupEntryKind::Documentation => LookupKind::Documentation,
            },
            availability: match e.availability {
                tidepool_runtime::session::InspectionAvailability::Available => {
                    LookupAvailability::Available
                }
                tidepool_runtime::session::InspectionAvailability::Polymorphic => {
                    LookupAvailability::Polymorphic
                }
                tidepool_runtime::session::InspectionAvailability::Unknown => {
                    LookupAvailability::Unknown
                }
                tidepool_runtime::session::InspectionAvailability::Unavailable => {
                    LookupAvailability::Unavailable
                }
            },
            origin: match e.origin {
                lookup_tool::LookupOrigin::ModuleExport => LookupOrigin::ModuleExport,
                lookup_tool::LookupOrigin::LiveBinding => LookupOrigin::LiveBinding,
                lookup_tool::LookupOrigin::Documentation => LookupOrigin::DocumentationOrigin,
            },
            quality: match e.quality {
                lookup_tool::MatchQuality::Exact => LookupQuality::Exact,
                lookup_tool::MatchQuality::Usable => LookupQuality::Usable,
            },
            usage: e.usage_pointer,
            example: None,
        }
    }
}
impl From<lookup_tool::LookupResult> for LookupResult {
    fn from(r: lookup_tool::LookupResult) -> Self {
        Self {
            query: r.query,
            outcome: match r.outcome {
                lookup_tool::LookupOutcome::Found { matches, truncated } => {
                    LookupOutcome::Found(matches.into_iter().map(Into::into).collect(), truncated)
                }
                lookup_tool::LookupOutcome::Ambiguous { matches, truncated } => {
                    LookupOutcome::Ambiguous(
                        matches.into_iter().map(Into::into).collect(),
                        truncated,
                    )
                }
                lookup_tool::LookupOutcome::NotFound {
                    attempted,
                    suggestions,
                } => LookupOutcome::Missing(
                    attempted.into_iter().map(|a| a.miss().to_owned()).collect(),
                    suggestions,
                ),
                lookup_tool::LookupOutcome::Rejected { diagnostic } => LookupOutcome::Rejected(
                    lookup_tool::strip_generated_query_locations(&diagnostic),
                ),
            },
        }
    }
}
pub(crate) fn queries(
    prepared: &[lookup_tool::PreparedLookup],
    imports: &str,
) -> Vec<InspectionQuery> {
    prepared
        .iter()
        .flat_map(|q| match &q.kind {
            lookup_tool::PreparedLookupKind::Name(name) => {
                match lookup_tool::qualifier_and_identifier(name) {
                    Some((qualifier, _)) => vec![
                        InspectionQuery::Info(name.clone()),
                        InspectionQuery::Browse {
                            module: lookup_tool::resolve_qualifier_module(imports, qualifier)
                                .unwrap_or_else(|| qualifier.into()),
                            expanded: false,
                        },
                    ],
                    None => vec![InspectionQuery::Info(name.clone())],
                }
            }
            lookup_tool::PreparedLookupKind::Qualified(name) => vec![
                InspectionQuery::Info(name.clone()),
                InspectionQuery::Browse {
                    module: name.clone(),
                    expanded: false,
                },
            ],
            lookup_tool::PreparedLookupKind::Type(query) => {
                vec![InspectionQuery::TypeSearch(query.clone())]
            }
            lookup_tool::PreparedLookupKind::Doc(_)
            | lookup_tool::PreparedLookupKind::Rejected(_) => vec![],
        })
        .collect()
}

struct ParentExportProbe {
    answer_index: usize,
    name: String,
    leaf: String,
    result_index: usize,
}

struct LookupInspectionPlan {
    queries: Vec<InspectionQuery>,
    primary_len: usize,
    parent_exports: Vec<ParentExportProbe>,
    reference_indices: Vec<usize>,
}

impl LookupInspectionPlan {
    fn new(
        prepared: &[lookup_tool::PreparedLookup],
        imports: &str,
        references: &[LookupReference],
    ) -> Self {
        let primary = queries(prepared, imports);
        let mut plan = Self {
            primary_len: primary.len(),
            queries: primary,
            parent_exports: vec![],
            reference_indices: vec![],
        };
        let mut answer_index = 0;
        for query in prepared {
            if let lookup_tool::PreparedLookupKind::Qualified(name) = &query.kind {
                if let Some((qualifier, leaf)) = name.rsplit_once('.') {
                    let module = lookup_tool::resolve_qualifier_module(imports, qualifier)
                        .unwrap_or_else(|| qualifier.into());
                    let result_index = plan.include(InspectionQuery::Browse {
                        module,
                        expanded: false,
                    });
                    plan.parent_exports.push(ParentExportProbe {
                        answer_index,
                        name: name.clone(),
                        leaf: leaf.to_owned(),
                        result_index,
                    });
                }
            }
            answer_index += queries(std::slice::from_ref(query), imports).len();
        }
        for reference in references {
            let result_index = plan.include(if reference.module.is_empty() {
                InspectionQuery::ScopeBrowse
            } else {
                InspectionQuery::Browse {
                    module: reference.module.clone(),
                    expanded: true,
                }
            });
            plan.reference_indices.push(result_index);
        }
        plan
    }

    fn include(&mut self, query: InspectionQuery) -> usize {
        if let Some(index) = self.queries.iter().position(|existing| *existing == query) {
            return index;
        }
        let index = self.queries.len();
        self.queries.push(query);
        index
    }
}

/// Whether resolving this request can submit any inspection batch. This is
/// shared with the resident workbench so documentation-only requests can
/// finish inline, while references and discovery-capable misses retain the
/// same inspection path as ordinary names.
pub(crate) fn has_inspection_work(request: &LookupRequest, imports: &str) -> bool {
    if !request.references.is_empty() {
        return true;
    }
    if request.queries.is_empty() {
        return false;
    }
    let Ok(prepared) =
        lookup_tool::prepare(serde_json::json!({"queries": request.queries.clone()}))
    else {
        return false;
    };
    !queries(&prepared, imports).is_empty()
}

pub(crate) fn requires_inspection(request: &LookupRequest, imports: &str, view: &str) -> bool {
    !request
        .expected_view
        .as_ref()
        .is_some_and(|expected| expected != view)
        && has_inspection_work(request, imports)
}

/// Bare names bound only while a typed request is being presented to this
/// actor (`Tidepool.RequestWorkbenchScope`'s preamble, mounted only for the
/// `Interactive` workbench). A miss on one of these looks exactly like any
/// other unresolved name; `note_request_only_bindings` names the reason when
/// no request is currently outstanding.
pub(crate) const REQUEST_ONLY_BINDINGS: [&str; 4] =
    ["respond", "sessionReply", "sessionInput", "reportProgress"];

/// Append a hint to a miss on a request-only binding, queried while no
/// typed request is outstanding for this actor. Call only when the caller
/// has confirmed there is no such request; a lookup made while one is
/// outstanding already resolves these names.
pub(crate) fn note_request_only_bindings(batch: &mut LookupBatch) {
    for result in &mut batch.results {
        if REQUEST_ONLY_BINDINGS.contains(&result.query.as_str()) {
            if let LookupOutcome::Missing(attempted, _) = &mut result.outcome {
                attempted.push("mounted only while a request is pending".to_owned());
            }
        }
    }
}

pub(crate) fn execute(
    request: LookupRequest,
    view: String,
    imports: &str,
    live_modules: &[String],
    workspace_modules: &[String],
    usage: crate::UsagePointerTable,
    inspect: impl Fn(&[InspectionQuery]) -> Result<Vec<InspectionResult>, LookupInspectionError>,
) -> LookupBatch {
    let mut batch = LookupBatch {
        results: vec![],
        candidates: vec![],
        view,
        issue: None,
    };
    if request
        .expected_view
        .as_ref()
        .is_some_and(|expected| expected != &batch.view)
    {
        batch.issue = Some("lookup compile view changed".into());
        return batch;
    }
    let prepared = if request.queries.is_empty() && !request.references.is_empty() {
        vec![]
    } else {
        match lookup_tool::prepare(serde_json::json!({"queries":request.queries})) {
            Ok(prepared) => prepared,
            Err(error) => {
                batch.issue = Some(error.to_string());
                return batch;
            }
        }
    };
    // Parent exports and explicit references are known before inspection.
    // Request their facts with the primary queries so ordinary browsing shares
    // one PreserveSource environment; type searches keep their own transform.
    let plan = LookupInspectionPlan::new(&prepared, imports, &request.references);
    let InspectionAnswers {
        results: mut inspected,
        unavailable,
    } = if plan.queries.is_empty() {
        InspectionAnswers {
            results: vec![],
            unavailable: None,
        }
    } else {
        match inspect_checked(&plan.queries, &inspect) {
            Ok(results) => InspectionAnswers {
                results,
                unavailable: None,
            },
            Err(InspectionFailure::SourceRejected(diagnostic)) => isolate(
                &prepared,
                imports,
                &plan.queries[plan.primary_len..],
                &inspect,
                &diagnostic,
            ),
            Err(InspectionFailure::Unavailable(diagnostic)) => InspectionAnswers {
                results: rejected(plan.queries.len(), &diagnostic),
                unavailable: Some(diagnostic),
            },
        }
    };
    batch.issue = unavailable;
    // Public exports are valid read-only lookup targets even without a source import.
    for index in 0..plan.primary_len.saturating_sub(1) {
        if let (
            InspectionQuery::Info(name),
            InspectionResult::NotFound { .. },
            InspectionResult::Browse { entries, .. },
        ) = (
            &plan.queries[index],
            &inspected[index],
            &inspected[index + 1],
        ) {
            if let Some((_, leaf)) = lookup_tool::qualifier_and_identifier(name) {
                let exact: Vec<_> = entries
                    .iter()
                    .filter(|entry| entry.name == leaf)
                    .cloned()
                    .collect();
                if !exact.is_empty() {
                    inspected[index] = if exact.len() > 1 {
                        InspectionResult::Ambiguous {
                            query: name.clone(),
                            entries: exact,
                        }
                    } else {
                        InspectionResult::Info {
                            query: name.clone(),
                            entries: exact,
                        }
                    };
                }
            }
        }
    }
    for probe in &plan.parent_exports {
        if matches!(
            inspected[probe.answer_index],
            InspectionResult::NotFound { .. }
        ) {
            if let InspectionResult::Browse { entries, .. } = &inspected[probe.result_index] {
                let exact = entries
                    .iter()
                    .filter(|entry| entry.name == probe.leaf)
                    .cloned()
                    .collect::<Vec<_>>();
                if !exact.is_empty() {
                    inspected[probe.answer_index] = if exact.len() > 1 {
                        InspectionResult::Ambiguous {
                            query: probe.name.clone(),
                            entries: exact,
                        }
                    } else {
                        InspectionResult::Info {
                            query: probe.name.clone(),
                            entries: exact,
                        }
                    };
                }
            }
        }
    }
    let reference_results = plan
        .reference_indices
        .iter()
        .map(|index| inspected[*index].clone())
        .collect::<Vec<_>>();
    inspected.truncate(plan.primary_len);
    let mut response = lookup_tool::resolve(
        prepared.clone(),
        inspected.clone(),
        live_modules,
        workspace_modules,
        usage.clone(),
    );
    for (index, reference) in request.references.iter().enumerate() {
        let query = if reference.module.is_empty() {
            reference.name.clone()
        } else {
            format!("{}.{}", reference.module, reference.name)
        };
        let outcome = match reference_results.get(index).cloned() {
            Some(InspectionResult::Browse { entries, .. }) => {
                let exact = entries
                    .into_iter()
                    .filter(|entry| {
                        entry.name == reference.name
                            && LookupNamespace::from_kind(&entry.kind) == reference.namespace
                    })
                    .collect::<Vec<_>>();
                if exact.is_empty() {
                    lookup_tool::LookupOutcome::NotFound {
                        attempted: vec![lookup_tool::LookupInterpretation::Name],
                        suggestions: vec![],
                    }
                } else {
                    let prepared = vec![lookup_tool::PreparedLookup {
                        query: query.clone(),
                        kind: lookup_tool::PreparedLookupKind::Name(reference.name.clone()),
                    }];
                    let inspected = if exact.len() > 1 {
                        InspectionResult::Ambiguous {
                            query: query.clone(),
                            entries: exact,
                        }
                    } else {
                        InspectionResult::Info {
                            query: query.clone(),
                            entries: exact,
                        }
                    };
                    lookup_tool::resolve(
                        prepared,
                        vec![inspected],
                        live_modules,
                        workspace_modules,
                        usage.clone(),
                    )
                    .results
                    .remove(0)
                    .outcome
                }
            }
            Some(InspectionResult::Rejected { diagnostic }) => {
                lookup_tool::LookupOutcome::Rejected { diagnostic }
            }
            Some(other) => lookup_tool::LookupOutcome::Rejected {
                diagnostic: other.render(),
            },
            None => lookup_tool::LookupOutcome::Rejected {
                diagnostic: "lookup compiler omitted reference result".into(),
            },
        };
        response
            .results
            .push(lookup_tool::LookupResult { query, outcome });
    }
    let cap = request.candidate_limit.clamp(0, 128) as usize;
    if request.discover && cap > 0 {
        let mut groups: Vec<Vec<LookupCandidate>> = vec![];
        let mut cursor = 0;
        let originals: Vec<_> = response
            .results
            .iter()
            .flat_map(|r| match &r.outcome {
                lookup_tool::LookupOutcome::Found { matches, .. } => matches
                    .iter()
                    .filter_map(|e| {
                        e.defining_module.as_ref().map(|module| LookupReference {
                            module: module.clone(),
                            name: e.name.clone(),
                            namespace: match e.kind {
                                lookup_tool::LookupEntryKind::Type => LookupNamespace::Type,
                                lookup_tool::LookupEntryKind::Constructor => {
                                    LookupNamespace::Constructor
                                }
                                lookup_tool::LookupEntryKind::RecordSelector => {
                                    LookupNamespace::Field
                                }
                                _ => LookupNamespace::Value,
                            },
                        })
                    })
                    .collect(),
                _ => Vec::new(),
            })
            .collect();
        let mut discovery_modules: Vec<String> = vec![];
        for query in &prepared {
            if let lookup_tool::PreparedLookupKind::Name(name)
            | lookup_tool::PreparedLookupKind::Qualified(name) = &query.kind
            {
                if let Some((qualifier, _)) = name.rsplit_once('.') {
                    let module = lookup_tool::resolve_qualifier_module(imports, qualifier)
                        .unwrap_or_else(|| qualifier.into());
                    if !discovery_modules.contains(&module) {
                        discovery_modules.push(module);
                    }
                }
            }
        }
        let named_count = discovery_modules.len();
        let local_count = named_count + 1;
        for module in workspace_modules {
            if !discovery_modules.contains(module) {
                discovery_modules.push(module.clone());
            }
        }
        // Module metadata is compiler-owned. Discovery is bounded independently of display.
        discovery_modules.truncate(128);
        let misses = response.results.iter().zip(&prepared).any(|(r, q)| {
            matches!(r.outcome, lookup_tool::LookupOutcome::NotFound { .. })
                && matches!(
                    q.kind,
                    lookup_tool::PreparedLookupKind::Name(_)
                        | lookup_tool::PreparedLookupKind::Qualified(_)
                )
        });
        let exports = if misses && batch.issue.is_none() {
            let mut discovery_queries = discovery_modules
                .iter()
                .map(|module| InspectionQuery::Browse {
                    module: module.clone(),
                    expanded: true,
                })
                .collect::<Vec<_>>();
            discovery_queries.insert(
                named_count.min(discovery_queries.len()),
                InspectionQuery::ScopeBrowse,
            );
            match inspect_checked(&discovery_queries, &inspect) {
                Ok(results) => results,
                Err(error) => {
                    batch.issue = Some(bound_diagnostic(
                        &format!(
                            "lookup candidate discovery unavailable: {}",
                            error.diagnostic()
                        ),
                        DIAGNOSTIC_BOUND,
                    ));
                    vec![]
                }
            }
        } else {
            vec![]
        };
        for (query, result) in prepared.iter().zip(&response.results) {
            let count = queries(std::slice::from_ref(query), imports).len();
            let own = &inspected[cursor..cursor + count];
            cursor += count;
            let mut candidates = vec![];
            if matches!(
                query.kind,
                lookup_tool::PreparedLookupKind::Doc(_)
                    | lookup_tool::PreparedLookupKind::Rejected(_)
            ) {
                groups.push(candidates);
                continue;
            }
            match &result.outcome {
                lookup_tool::LookupOutcome::Found {
                    matches: returned, ..
                } => {
                    if let Some(first) = own.first() {
                        let refs: Vec<_> = match first {
                            InspectionResult::Info { entries, .. } => entries
                                .iter()
                                .filter(|e| {
                                    returned
                                        .iter()
                                        .any(|r| r.name == e.name && r.defining_module == e.module)
                                })
                                .flat_map(|e| e.references.iter())
                                .collect(),
                            InspectionResult::TypeMatches { matches, .. } => matches
                                .iter()
                                .filter(|e| {
                                    returned
                                        .iter()
                                        .any(|r| r.name == e.name && r.defining_module == e.module)
                                })
                                .flat_map(|e| e.references.iter())
                                .collect(),
                            _ => vec![],
                        };
                        for reference in refs {
                            let qualified = if reference.module.is_empty() {
                                reference.name.clone()
                            } else {
                                format!("{}.{}", reference.module, reference.name)
                            };
                            if !originals.contains(&LookupReference::from(reference)) {
                                candidates.push(LookupCandidate {
                                    query: qualified,
                                    origins: vec![query.query.clone()],
                                    summary: format!(
                                        "{:?} referenced by {}",
                                        reference.namespace, query.query
                                    ),
                                    local: true,
                                    reference: Some(reference.into()),
                                });
                            }
                        }
                    }
                }
                lookup_tool::LookupOutcome::Ambiguous { matches, .. } => {
                    for entry in matches {
                        candidates.push(LookupCandidate {
                            query: entry.defining_module.as_ref().map_or_else(
                                || entry.name.clone(),
                                |m| format!("{m}.{}", entry.name),
                            ),
                            origins: vec![query.query.clone()],
                            summary: entry.signature_or_declaration.clone(),
                            local: true,
                            reference: entry.defining_module.as_ref().map(|module| {
                                LookupReference {
                                    module: module.clone(),
                                    name: entry.name.clone(),
                                    namespace: match entry.kind {
                                        lookup_tool::LookupEntryKind::Type => LookupNamespace::Type,
                                        lookup_tool::LookupEntryKind::Constructor => {
                                            LookupNamespace::Constructor
                                        }
                                        lookup_tool::LookupEntryKind::RecordSelector => {
                                            LookupNamespace::Field
                                        }
                                        _ => LookupNamespace::Value,
                                    },
                                }
                            }),
                        });
                    }
                }
                lookup_tool::LookupOutcome::NotFound { .. }
                    if matches!(
                        query.kind,
                        lookup_tool::PreparedLookupKind::Name(_)
                            | lookup_tool::PreparedLookupKind::Qualified(_)
                    ) =>
                {
                    for (index, export) in exports.iter().enumerate() {
                        if let InspectionResult::Browse {
                            module, entries, ..
                        } = export
                        {
                            for entry in entries {
                                let module = if module.is_empty() {
                                    entry.module.as_deref().unwrap_or("")
                                } else {
                                    module.as_str()
                                };
                                candidates.push(LookupCandidate {
                                    query: if module.is_empty() {
                                        entry.name.clone()
                                    } else {
                                        format!("{module}.{}", entry.name)
                                    },
                                    origins: vec![query.query.clone()],
                                    summary: entry.display.clone(),
                                    local: index < local_count,
                                    reference: Some(LookupReference {
                                        module: module.to_owned(),
                                        name: entry.name.clone(),
                                        namespace: LookupNamespace::from_kind(&entry.kind),
                                    }),
                                });
                            }
                        }
                    }
                }
                _ => {}
            }
            if matches!(result.outcome, lookup_tool::LookupOutcome::NotFound { .. }) {
                let leaf = query.query.rsplit('.').next().unwrap_or(&query.query);
                candidates.sort_by_key(|candidate| {
                    (
                        !candidate.local,
                        lookup_tool::edit_distance(
                            leaf,
                            candidate
                                .query
                                .rsplit('.')
                                .next()
                                .unwrap_or(&candidate.query),
                        ),
                    )
                });
            }
            groups.push(candidates);
        }
        // Round-robin admission prevents the first query monopolizing the shared budget.
        let mut groups: Vec<_> = groups.into_iter().map(Vec::into_iter).collect();
        while batch.candidates.len() < cap {
            let mut progressed = false;
            for group in &mut groups {
                if let Some(candidate) = group.next() {
                    progressed = true;
                    if let Some(existing) = batch
                        .candidates
                        .iter_mut()
                        .find(|c| c.query == candidate.query && c.reference == candidate.reference)
                    {
                        for origin in candidate.origins {
                            if !existing.origins.contains(&origin) {
                                existing.origins.push(origin);
                            }
                        }
                    } else {
                        batch.candidates.push(candidate);
                    }
                    if batch.candidates.len() == cap {
                        break;
                    }
                }
            }
            if !progressed {
                break;
            }
        }
        // A full budget limits declarations, not the original queries linked to them.
        for candidate in groups.into_iter().flatten() {
            if let Some(existing) = batch.candidates.iter_mut().find(|existing| {
                existing.query == candidate.query && existing.reference == candidate.reference
            }) {
                for origin in candidate.origins {
                    if !existing.origins.contains(&origin) {
                        existing.origins.push(origin);
                    }
                }
            }
        }
    }
    batch.results = response.results.into_iter().map(Into::into).collect();
    // Only original direct queries can carry examples. Related references are
    // resolved separately and remain declaration-only, even on a raw batch.
    for result in batch.results.iter_mut().take(prepared.len()) {
        let LookupOutcome::Found(entries, false) = &mut result.outcome else {
            continue;
        };
        let [entry] = entries.as_mut_slice() else {
            continue;
        };
        if !matches!(&entry.origin, LookupOrigin::ModuleExport)
            || matches!(&entry.availability, LookupAvailability::Unavailable)
        {
            continue;
        }
        let Some(module) = entry.module.as_deref() else {
            continue;
        };
        let namespace = match &entry.kind {
            LookupKind::Value | LookupKind::ClassMethod => {
                crate::usage_pointer::ExampleNamespace::Value
            }
            LookupKind::Type => crate::usage_pointer::ExampleNamespace::Type,
            LookupKind::Constructor => crate::usage_pointer::ExampleNamespace::Constructor,
            LookupKind::RecordSelector => crate::usage_pointer::ExampleNamespace::Field,
            LookupKind::Coercion | LookupKind::Documentation => continue,
        };
        entry.example = crate::usage_pointer::example_for(&usage, module, &entry.name, namespace);
    }
    batch
}

/// Bound a diagnostic to a display-worthy length. A worker-failure message
/// can carry stderr straight through (see `CompileError::WorkerFailure`);
/// nothing here re-truncates cleanly at a char boundary, and no lookup
/// diagnostic should grow unbounded inside a per-query result.
fn bound_diagnostic(diagnostic: &str, limit: usize) -> String {
    if diagnostic.chars().count() <= limit {
        diagnostic.to_owned()
    } else {
        let truncated: String = diagnostic.chars().take(limit).collect();
        format!("{truncated}… (truncated)")
    }
}

const DIAGNOSTIC_BOUND: usize = 2_000;

enum InspectionFailure {
    SourceRejected(String),
    Unavailable(String),
}

impl InspectionFailure {
    fn diagnostic(&self) -> &str {
        match self {
            Self::SourceRejected(diagnostic) | Self::Unavailable(diagnostic) => diagnostic,
        }
    }
}

struct InspectionAnswers {
    results: Vec<InspectionResult>,
    unavailable: Option<String>,
}

fn rejected(count: usize, diagnostic: &str) -> Vec<InspectionResult> {
    let diagnostic = bound_diagnostic(diagnostic, DIAGNOSTIC_BOUND);
    (0..count)
        .map(|_| InspectionResult::Rejected {
            diagnostic: diagnostic.clone(),
        })
        .collect()
}

fn inspect_checked(
    queries: &[InspectionQuery],
    inspect: &impl Fn(&[InspectionQuery]) -> Result<Vec<InspectionResult>, LookupInspectionError>,
) -> Result<Vec<InspectionResult>, InspectionFailure> {
    match inspect(queries) {
        Ok(values) if values.len() == queries.len() => Ok(values),
        Ok(values) => Err(InspectionFailure::Unavailable(format!(
            "lookup compiler returned {} results for {} queries",
            values.len(),
            queries.len()
        ))),
        Err(error) => {
            let diagnostic =
                bound_diagnostic(&render_lookup_inspection_error(&error), DIAGNOSTIC_BOUND);
            // Only GHC source rejection can be narrowed by partitioning queries.
            // Worker, admission, cancellation and receipt failures apply to the
            // inspection service; submitting more queries cannot isolate them.
            match error {
                LookupInspectionError::Compiler(tidepool_runtime::CompileError::Diagnostics(_)) => {
                    Err(InspectionFailure::SourceRejected(diagnostic))
                }
                _ => Err(InspectionFailure::Unavailable(diagnostic)),
            }
        }
    }
}

fn isolate(
    prepared: &[lookup_tool::PreparedLookup],
    imports: &str,
    supplemental: &[InspectionQuery],
    inspect: &impl Fn(&[InspectionQuery]) -> Result<Vec<InspectionResult>, LookupInspectionError>,
    original_diagnostic: &str,
) -> InspectionAnswers {
    let parts = prepared
        .iter()
        .map(|query| queries(std::slice::from_ref(query), imports))
        .filter(|queries| !queries.is_empty())
        .chain(supplemental.iter().map(|query| vec![query.clone()]))
        .collect::<Vec<_>>();
    if parts.len() == 1 {
        // Repeating the identical request would provide no narrower evidence.
        return InspectionAnswers {
            results: rejected(parts[0].len(), original_diagnostic),
            unavailable: None,
        };
    }
    let mut results = Vec::new();
    let mut unavailable: Option<String> = None;
    for queries in parts {
        if let Some(diagnostic) = &unavailable {
            results.extend(rejected(queries.len(), diagnostic));
            continue;
        }
        match inspect_checked(&queries, inspect) {
            Ok(values) => results.extend(values),
            Err(InspectionFailure::SourceRejected(diagnostic)) => {
                results.extend(rejected(queries.len(), &diagnostic))
            }
            Err(InspectionFailure::Unavailable(diagnostic)) => {
                results.extend(rejected(queries.len(), &diagnostic));
                unavailable = Some(diagnostic);
            }
        }
    }
    InspectionAnswers {
        results,
        unavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use tidepool_runtime::session::{
        IdentifierNamespace, IdentifierRef, InfoEntry, InspectionAvailability,
    };

    #[test]
    fn compiler_diagnostics_keep_details_until_lookup_outcome_rendering() {
        let error =
            LookupInspectionError::Compiler(tidepool_runtime::CompileError::WorkerFailure(vec![
                tidepool_toolchain::diag::ExtractDiag {
                    span: Some(tidepool_toolchain::diag::DiagSpan {
                        file: "/tmp/query/Expr.hs".into(),
                        start_line: 57,
                        start_col: 9,
                        end_line: 57,
                        end_col: 21,
                    }),
                    severity: tidepool_toolchain::diag::DiagnosticSeverity::Error,
                    message:
                        "lookup module did not expose __tidepool_lookup_query\nsecondary detail"
                            .into(),
                },
                tidepool_toolchain::diag::ExtractDiag {
                    span: None,
                    severity: tidepool_toolchain::diag::DiagnosticSeverity::Warning,
                    message: "worker recovery detail".into(),
                },
            ]));

        let rendered = render_lookup_inspection_error(&error);
        assert!(
            rendered.starts_with("lookup compiler worker failed:"),
            "{rendered}"
        );
        assert!(
            rendered.contains(
                "error: lookup module did not expose __tidepool_lookup_query\nsecondary detail"
            ),
            "{rendered}"
        );
        assert!(
            rendered.contains("warning: worker recovery detail"),
            "{rendered}"
        );
        assert!(!rendered.contains("/tmp/query/Expr.hs"), "{rendered}");

        let source_error =
            LookupInspectionError::Compiler(tidepool_runtime::CompileError::Diagnostics(vec![
                tidepool_toolchain::diag::ExtractDiag {
                    span: Some(tidepool_toolchain::diag::DiagSpan {
                        file: "src/Query.hs".into(),
                        start_line: 57,
                        start_col: 9,
                        end_line: 57,
                        end_col: 21,
                    }),
                    severity: tidepool_toolchain::diag::DiagnosticSeverity::Error,
                    message: "source-level type error".into(),
                },
            ]));
        assert!(render_lookup_inspection_error(&source_error)
            .contains("src/Query.hs:57:9-57:21: error: source-level type error"));
    }

    fn source_error(message: &str) -> LookupInspectionError {
        LookupInspectionError::Compiler(tidepool_runtime::CompileError::Diagnostics(vec![
            tidepool_toolchain::diag::ExtractDiag {
                span: None,
                severity: tidepool_toolchain::diag::DiagnosticSeverity::Error,
                message: message.into(),
            },
        ]))
    }

    fn entry(name: &str) -> InfoEntry {
        InfoEntry {
            name: name.into(),
            module: Some("Project.Test".into()),
            kind: "value".into(),
            display: format!("{name} :: Int"),
            availability: InspectionAvailability::Available,
            references: vec![],
        }
    }
    fn request(queries: &[&str], discover: bool) -> LookupRequest {
        LookupRequest {
            queries: queries.iter().map(|s| (*s).into()).collect(),
            discover,
            expected_view: None,
            candidate_limit: 128,
            references: vec![],
        }
    }
    #[test]
    fn qualified_module_parent_names_and_references_share_inspection() {
        let calls = Cell::new(0);
        let mut req = request(&["Project.Work", "Project.Choice", "doc workbench"], false);
        req.references.push(LookupReference {
            module: "Project".into(),
            name: "Choice".into(),
            namespace: LookupNamespace::Constructor,
        });
        let result = execute(
            req,
            "view".into(),
            "",
            &[],
            &[],
            crate::UsagePointerTable::default(),
            |queries| {
                calls.set(calls.get() + 1);
                Ok(queries
                    .iter()
                    .map(|query| match query {
                        InspectionQuery::Info(name) => InspectionResult::NotFound {
                            query: name.clone(),
                        },
                        InspectionQuery::Browse { module, expanded } => {
                            let entries = match module.as_str() {
                                "Project" => {
                                    let mut ty = entry("Choice");
                                    ty.kind = "type".into();
                                    let mut constructor = entry("Choice");
                                    constructor.kind = "constructor".into();
                                    vec![ty, constructor]
                                }
                                "Project.Work" => vec![entry("runWork")],
                                "Project.Choice" => vec![entry("moduleChoice")],
                                _ => panic!("unexpected module {module}"),
                            };
                            InspectionResult::Browse {
                                module: module.clone(),
                                expanded: *expanded,
                                entries,
                            }
                        }
                        _ => panic!("unexpected query {query:?}"),
                    })
                    .collect())
            },
        );
        assert_eq!(
            calls.get(),
            1,
            "known export facts must not recompile the view"
        );
        let LookupOutcome::Found(entries, false) = &result.results[0].outcome else {
            panic!("module browse must succeed")
        };
        assert_eq!(entries[0].name, "runWork");
        let LookupOutcome::Ambiguous(entries, false) = &result.results[1].outcome else {
            panic!("parent names take precedence over a successful module browse")
        };
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|entry| entry.name == "Choice"));
        let LookupOutcome::Found(entries, false) = &result.results[2].outcome else {
            panic!("local documentation must resolve")
        };
        assert!(matches!(entries[0].kind, LookupKind::Documentation));
        let LookupOutcome::Found(entries, false) = &result.results[3].outcome else {
            panic!("explicit reference must resolve")
        };
        assert_eq!(entries.len(), 1);
        assert!(matches!(entries[0].kind, LookupKind::Constructor));
    }

    #[test]
    fn supplemental_inspection_failures_do_not_discard_primary_answers() {
        let mut req = request(&["Project.Work", "x"], false);
        req.references.push(LookupReference {
            module: "Broken.Reference".into(),
            name: "missing".into(),
            namespace: LookupNamespace::Value,
        });
        let result = execute(
            req,
            "view".into(),
            "",
            &[],
            &[],
            crate::UsagePointerTable::default(),
            |queries| {
                if queries.len() > 2 {
                    return Err(source_error("combined source rejected"));
                }
                queries
                    .iter()
                    .map(|query| match query {
                        InspectionQuery::Info(name) if name == "x" => Ok(InspectionResult::Info {
                            query: name.clone(),
                            entries: vec![entry("x")],
                        }),
                        InspectionQuery::Info(name) => Ok(InspectionResult::NotFound {
                            query: name.clone(),
                        }),
                        InspectionQuery::Browse { module, expanded }
                            if module == "Project.Work" =>
                        {
                            Ok(InspectionResult::Browse {
                                module: module.clone(),
                                expanded: *expanded,
                                entries: vec![entry("runWork")],
                            })
                        }
                        InspectionQuery::Browse { module, .. } if module == "Broken.Reference" => {
                            Err(source_error("reference source rejected"))
                        }
                        InspectionQuery::Browse { .. } => {
                            Err(source_error("parent source rejected"))
                        }
                        _ => panic!("unexpected query {query:?}"),
                    })
                    .collect()
            },
        );
        assert!(matches!(
            result.results[0].outcome,
            LookupOutcome::Found(_, false)
        ));
        assert!(matches!(
            result.results[1].outcome,
            LookupOutcome::Found(_, false)
        ));
        assert!(
            matches!(&result.results[2].outcome, LookupOutcome::Rejected(diagnostic)
            if diagnostic.contains("reference source rejected"))
        );
    }

    #[test]
    fn documentation_only_does_not_invoke_inspection() {
        let result = execute(
            request(&["doc workbench"], false),
            "view".into(),
            "",
            &[],
            &[],
            crate::UsagePointerTable::default(),
            |_| panic!("documentation does not need GHC"),
        );
        assert!(matches!(
            result.results[0].outcome,
            LookupOutcome::Found(_, false)
        ));
    }

    #[test]
    fn changed_view_performs_no_inspection() {
        let mut req = request(&["x"], true);
        req.expected_view = Some("old".into());
        let result = execute(
            req,
            "new".into(),
            "",
            &[],
            &[],
            crate::UsagePointerTable::default(),
            |_| panic!("changed view must not inspect"),
        );
        assert!(result.issue.is_some());
        assert!(result.results.is_empty());
    }

    #[test]
    fn inspection_admission_skips_docs_and_pre_rejected_requests() {
        let view = "current";
        assert!(!has_inspection_work(
            &request(&["doc workbench"], false),
            ""
        ));
        assert!(!has_inspection_work(&request(&[" "], false), ""));
        assert!(!has_inspection_work(&request(&[""], false), ""));
        assert!(requires_inspection(
            &request(&["pollResponse"], false),
            "",
            view
        ));
        assert!(requires_inspection(
            &request(&[":: Int -> Int"], false),
            "",
            view
        ));

        let mut reference_only = request(&[], false);
        reference_only.references.push(LookupReference {
            module: "Tidepool.Effects.Core".into(),
            name: "LookupBatch".into(),
            namespace: LookupNamespace::Type,
        });
        assert!(requires_inspection(&reference_only, "", view));

        let mut changed = request(&["pollResponse"], false);
        changed.expected_view = Some("old".into());
        assert!(!requires_inspection(&changed, "", view));
    }

    #[test]
    fn raw_lookup_never_discovers_and_preserves_outcome() {
        let calls = Cell::new(0);
        let result = execute(
            request(&["x"], false),
            "view".into(),
            "",
            &[],
            &["Project.Test".into()],
            crate::UsagePointerTable::default(),
            |queries| {
                calls.set(calls.get() + 1);
                assert_eq!(queries.len(), 1);
                Ok(vec![InspectionResult::Info {
                    query: "x".into(),
                    entries: vec![entry("x")],
                }])
            },
        );
        assert_eq!(calls.get(), 1);
        assert!(result.candidates.is_empty());
        assert!(matches!(
            result.results[0].outcome,
            LookupOutcome::Found(_, false)
        ));
    }
    #[test]
    fn examples_follow_resolved_identity_and_exclude_live_ambiguous_unavailable() {
        let workspace = tempfile::tempdir().unwrap();
        let checks = workspace.path().join(".exomonad/workspace/checks");
        std::fs::create_dir_all(&checks).unwrap();
        std::fs::write(checks.join("example.hs"), "Cmd.start command\n").unwrap();
        std::fs::write(
            checks.join("usage-examples.json"),
            r#"{"examples":[{
            "module":"Tidepool.Command","name":"start","namespace":"value",
            "source":"checks/example.hs","prerequisites":[],"requirements":"Import Cmd"
        }]}"#,
        )
        .unwrap();
        let usage = crate::UsagePointerTable::discover(workspace.path()).unwrap();
        let make_entry = |module: &str, availability| InfoEntry {
            name: "start".into(),
            module: Some(module.into()),
            kind: "value".into(),
            display: "start :: Command -> Eff effects Job".into(),
            availability,
            references: vec![],
        };
        let available = InspectionAvailability::Available;
        let found = execute(
            request(&["Cmd.start", "Alias.start"], false),
            "view".into(),
            "qualified Tidepool.Command as Cmd\nqualified Tidepool.Command as Alias",
            &[],
            &[],
            usage.clone(),
            |queries| {
                Ok(queries
                    .iter()
                    .map(|query| match query {
                        InspectionQuery::Info(name) => InspectionResult::Info {
                            query: name.clone(),
                            entries: vec![make_entry("Tidepool.Command", available)],
                        },
                        InspectionQuery::Browse { module, expanded } => InspectionResult::Browse {
                            module: module.clone(),
                            expanded: *expanded,
                            entries: vec![],
                        },
                        _ => panic!("unexpected query"),
                    })
                    .collect())
            },
        );
        for result in &found.results {
            let LookupOutcome::Found(entries, false) = &result.outcome else {
                panic!("expected one hit")
            };
            let [entry] = entries.as_slice() else {
                panic!("expected one entry")
            };
            assert_eq!(
                entry.example.as_ref().unwrap().source,
                "Cmd.start command\n"
            );
        }
        let excluded = execute(
            request(&["start"], false),
            "view".into(),
            "",
            &[],
            &[],
            usage.clone(),
            |_| {
                Ok(vec![InspectionResult::Info {
                    query: "start".into(),
                    entries: vec![make_entry("Other.Command", available)],
                }])
            },
        );
        let LookupOutcome::Found(entries, false) = &excluded.results[0].outcome else {
            panic!()
        };
        let [entry] = entries.as_slice() else {
            panic!()
        };
        assert!(entry.example.is_none());
        let live = execute(
            request(&["start"], false),
            "view".into(),
            "",
            &["Tidepool.Command".into()],
            &[],
            usage.clone(),
            |_| {
                Ok(vec![InspectionResult::Info {
                    query: "start".into(),
                    entries: vec![make_entry("Tidepool.Command", available)],
                }])
            },
        );
        let LookupOutcome::Found(entries, false) = &live.results[0].outcome else {
            panic!()
        };
        let [entry] = entries.as_slice() else {
            panic!()
        };
        assert!(entry.example.is_none());
        let unavailable = execute(
            request(&["start"], false),
            "view".into(),
            "",
            &[],
            &[],
            usage.clone(),
            |_| {
                Ok(vec![InspectionResult::Info {
                    query: "start".into(),
                    entries: vec![make_entry(
                        "Tidepool.Command",
                        InspectionAvailability::Unavailable,
                    )],
                }])
            },
        );
        let LookupOutcome::Found(entries, false) = &unavailable.results[0].outcome else {
            panic!()
        };
        let [entry] = entries.as_slice() else {
            panic!()
        };
        assert!(entry.example.is_none());
        let ambiguous = execute(
            request(&["start"], false),
            "view".into(),
            "",
            &[],
            &[],
            usage,
            |_| {
                Ok(vec![InspectionResult::Ambiguous {
                    query: "start".into(),
                    entries: vec![
                        make_entry("Tidepool.Command", available),
                        make_entry("Other.Command", available),
                    ],
                }])
            },
        );
        assert!(matches!(
            ambiguous.results[0].outcome,
            LookupOutcome::Ambiguous(_, _)
        ));
    }
    #[test]
    fn references_are_one_degree_deduplicated_and_fair() {
        let mut req = request(&["a", "b"], true);
        req.candidate_limit = 2;
        let result = execute(
            req,
            "view".into(),
            "",
            &[],
            &[],
            crate::UsagePointerTable::default(),
            |queries| {
                Ok(queries
                    .iter()
                    .map(|q| {
                        let InspectionQuery::Info(name) = q else {
                            panic!()
                        };
                        let mut e = entry(name);
                        e.references = vec![
                            IdentifierRef {
                                module: "Project.Test".into(),
                                name: format!("{name}First"),
                                namespace: IdentifierNamespace::Type,
                            },
                            IdentifierRef {
                                module: "Project.Test".into(),
                                name: format!("{name}Second"),
                                namespace: IdentifierNamespace::Type,
                            },
                        ];
                        InspectionResult::Info {
                            query: name.clone(),
                            entries: vec![e],
                        }
                    })
                    .collect())
            },
        );
        assert_eq!(
            result
                .candidates
                .iter()
                .map(|c| c.query.as_str())
                .collect::<Vec<_>>(),
            vec!["Project.Test.aFirst", "Project.Test.bFirst"]
        );
    }
    #[test]
    fn full_candidate_budget_retains_origins_from_later_queries() {
        let mut req = request(&["a", "b"], true);
        req.candidate_limit = 1;
        let result = execute(
            req,
            "view".into(),
            "",
            &[],
            &[],
            crate::UsagePointerTable::default(),
            |queries| {
                Ok(queries
                    .iter()
                    .map(|query| {
                        let InspectionQuery::Info(name) = query else {
                            panic!()
                        };
                        let mut entry = entry(name);
                        entry.references = vec![IdentifierRef {
                            module: "Project.Test".into(),
                            name: "Shared".into(),
                            namespace: IdentifierNamespace::Type,
                        }];
                        InspectionResult::Info {
                            query: name.clone(),
                            entries: vec![entry],
                        }
                    })
                    .collect())
            },
        );
        assert_eq!(result.candidates.len(), 1);
        assert_eq!(result.candidates[0].origins, vec!["a", "b"]);
    }
    #[test]
    fn missed_name_searches_imports_and_keeps_failure() {
        let result = execute(
            request(&["targte"], true),
            "view".into(),
            "qualified Project.Test as T",
            &[],
            &[],
            crate::UsagePointerTable::default(),
            |queries| {
                Ok(queries
                    .iter()
                    .map(|q| match q {
                        InspectionQuery::Info(name) => InspectionResult::NotFound {
                            query: name.clone(),
                        },
                        InspectionQuery::ScopeBrowse => InspectionResult::Browse {
                            module: String::new(),
                            expanded: true,
                            entries: vec![entry("aaa"), entry("target")],
                        },
                        _ => panic!(),
                    })
                    .collect())
            },
        );
        assert!(matches!(
            result.results[0].outcome,
            LookupOutcome::Missing(_, _)
        ));
        assert_eq!(result.candidates[0].query, "Project.Test.target");
        assert!(result.candidates[0].local);
    }
    #[test]
    fn type_miss_does_not_browse_modules() {
        let calls = Cell::new(0);
        let result = execute(
            request(&[":: Int -> Bool"], true),
            "view".into(),
            "Project.Test",
            &[],
            &[],
            crate::UsagePointerTable::default(),
            |queries| {
                calls.set(calls.get() + 1);
                assert!(queries
                    .iter()
                    .all(|q| matches!(q, InspectionQuery::TypeSearch(_))));
                Ok(vec![InspectionResult::TypeMatches {
                    query: "Int -> Bool".into(),
                    matches: vec![],
                }])
            },
        );
        assert!(result.candidates.is_empty());
        assert_eq!(calls.get(), 1);
    }
    /// A query-local GHC source failure preserves independently valid neighbors.
    #[test]
    fn mixed_batch_isolates_one_failing_query_from_its_neighbors() {
        let calls = Cell::new(0);
        let result = execute(
            request(&["validName", "unknownName", ":: Int -> Bool"], false),
            "view".into(),
            "",
            &[],
            &[],
            crate::UsagePointerTable::default(),
            |queries| {
                calls.set(calls.get() + 1);
                if queries.len() != 1 {
                    return Err(source_error("combined source rejected"));
                }
                match &queries[0] {
                    InspectionQuery::Info(name) if name == "validName" => {
                        Ok(vec![InspectionResult::Info {
                            query: name.clone(),
                            entries: vec![entry("validName")],
                        }])
                    }
                    InspectionQuery::Info(name) if name == "unknownName" => {
                        Ok(vec![InspectionResult::NotFound {
                            query: name.clone(),
                        }])
                    }
                    InspectionQuery::TypeSearch(_) => Err(source_error(
                        "Not in scope: type constructor or class `Bool'",
                    )),
                    other => panic!("unexpected retried query: {other:?}"),
                }
            },
        );
        // One combined attempt, then one retry per query.
        assert_eq!(calls.get(), 4);
        assert_eq!(result.results.len(), 3);
        assert!(matches!(
            result.results[0].outcome,
            LookupOutcome::Found(_, false)
        ));
        assert!(matches!(
            result.results[1].outcome,
            LookupOutcome::Missing(_, _)
        ));
        match &result.results[2].outcome {
            LookupOutcome::Rejected(diagnostic) => {
                assert!(diagnostic.contains("Not in scope"), "{diagnostic}");
            }
            _ => panic!("expected the signature query to be rejected"),
        }
    }
    fn missing(query: &str) -> LookupResult {
        LookupResult {
            query: query.into(),
            outcome: LookupOutcome::Missing(vec!["not in scope as a name".into()], vec![]),
        }
    }
    #[test]
    fn note_request_only_bindings_hints_a_miss_on_a_request_only_name() {
        let mut batch = LookupBatch {
            results: vec![missing("respond"), missing("otherName")],
            candidates: vec![],
            view: "view".into(),
            issue: None,
        };
        note_request_only_bindings(&mut batch);
        let LookupOutcome::Missing(attempted, _) = &batch.results[0].outcome else {
            panic!("expected a miss");
        };
        assert!(
            attempted
                .iter()
                .any(|line| line == "mounted only while a request is pending"),
            "{attempted:?}"
        );
        // An unrelated miss is untouched.
        let LookupOutcome::Missing(attempted, _) = &batch.results[1].outcome else {
            panic!("expected a miss");
        };
        assert_eq!(attempted, &["not in scope as a name"]);
    }
    #[test]
    fn note_request_only_bindings_leaves_a_found_request_only_name_alone() {
        let mut batch = LookupBatch {
            results: vec![LookupResult {
                query: "respond".into(),
                outcome: LookupOutcome::Found(vec![], false),
            }],
            candidates: vec![],
            view: "view".into(),
            issue: None,
        };
        note_request_only_bindings(&mut batch);
        assert!(matches!(
            batch.results[0].outcome,
            LookupOutcome::Found(_, false)
        ));
    }
}

#[cfg(test)]
mod reference_tests {
    use super::*;
    use tidepool_runtime::session::{InfoEntry, InspectionAvailability};
    #[test]
    fn exact_reference_preserves_constructor_namespace() {
        let result = execute(
            LookupRequest {
                queries: vec![],
                discover: false,
                expected_view: Some("view".into()),
                candidate_limit: 128,
                references: vec![LookupReference {
                    module: "Project.Test".into(),
                    name: "Same".into(),
                    namespace: LookupNamespace::Constructor,
                }],
            },
            "view".into(),
            "",
            &[],
            &[],
            crate::UsagePointerTable::default(),
            |queries| {
                Ok(queries
                    .iter()
                    .map(|q| {
                        assert!(matches!(q, InspectionQuery::Browse { expanded: true, .. }));
                        InspectionResult::Browse {
                            module: "Project.Test".into(),
                            expanded: true,
                            entries: ["type", "constructor"]
                                .into_iter()
                                .map(|kind| InfoEntry {
                                    name: "Same".into(),
                                    module: Some("Project.Test".into()),
                                    kind: kind.into(),
                                    display: kind.into(),
                                    availability: InspectionAvailability::Available,
                                    references: vec![],
                                })
                                .collect(),
                        }
                    })
                    .collect())
            },
        );
        let LookupOutcome::Found(entries, false) = &result.results[0].outcome else {
            panic!("exact reference did not resolve")
        };
        assert_eq!(entries.len(), 1);
        assert!(matches!(entries[0].kind, LookupKind::Constructor));
    }
}

#[cfg(test)]
#[path = "lookup/inspection_failure_tests.rs"]
mod inspection_failure_tests;
