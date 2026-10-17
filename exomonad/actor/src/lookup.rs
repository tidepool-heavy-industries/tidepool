//! Structured lookup mechanics shared by the self-hosted lookup tool.
use crate::lookup_tool;
use tidepool_bridge_derive::{FromHaskell, ToHaskell};
use tidepool_extract_cmd::{
    ExactNameInspection, InspectionNamespace, InspectionScope, LookupNameInspection,
};
use tidepool_runtime::session::inspection::{
    HostedFallback, HostedLookupAnswer, HostedLookupBatch, HostedLookupDiscovery,
    HostedLookupOperation, HostedLookupOutput, HostedLookupQuery, HostedOperationIdentity,
};
use tidepool_runtime::session::{InspectionQuery, LookupNameMode, NameScope};

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
    pub identity: String,
    pub search_receipt: LookupSearchReceipt,
}
#[derive(ToHaskell)]
#[haskell(module = "Tidepool.Effects.Core", name = "LookupSearchReceipt")]
pub(crate) struct LookupSearchReceipt {
    view: String,
    disposition: LookupSearchDisposition,
    attempted_scopes: Vec<LookupSearchScope>,
    matches: Vec<LookupMatchReceipt>,
    discovery: LookupDiscovery,
}
impl LookupSearchReceipt {
    fn new(view: String, discover: bool) -> Self {
        Self {
            view,
            disposition: LookupSearchDisposition::NotExecuted,
            attempted_scopes: vec![],
            matches: vec![],
            discovery: if discover {
                LookupDiscovery::NotRun(LookupSearchNotRun::Unavailable)
            } else {
                LookupDiscovery::NotRequested
            },
        }
    }
}
#[derive(ToHaskell)]
pub(crate) enum LookupSearchDisposition {
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupSearchNotExecuted")]
    NotExecuted,
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupSearchCaptured")]
    Captured,
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupSearchInvalidated")]
    Invalidated(Vec<LookupMatchReceipt>, LookupDiscovery),
}
impl LookupBatch {
    /// Refuse stale facts while preserving whether work actually executed.
    pub(crate) fn invalidate_changed_view(&mut self) {
        if matches!(
            self.search_receipt.disposition,
            LookupSearchDisposition::Invalidated(..)
        ) {
            return;
        }
        self.results.clear();
        self.candidates.clear();
        self.issue = Some("lookup compile view changed".into());
        if matches!(
            self.search_receipt.disposition,
            LookupSearchDisposition::NotExecuted
        ) {
            if !matches!(self.search_receipt.discovery, LookupDiscovery::NotRequested) {
                self.search_receipt.discovery =
                    LookupDiscovery::NotRun(LookupSearchNotRun::StaleView);
            }
            return;
        }
        let observations = std::mem::take(&mut self.search_receipt.matches);
        let discovery = std::mem::replace(
            &mut self.search_receipt.discovery,
            LookupDiscovery::Invalidated,
        );
        self.search_receipt.disposition =
            LookupSearchDisposition::Invalidated(observations, discovery);
    }
}
#[derive(Clone, Debug, PartialEq, Eq, ToHaskell)]
pub(crate) enum LookupSearchScope {
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupCurrentScope")]
    Current,
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupPublicModule")]
    Module(String),
}
#[derive(Clone, Debug, PartialEq, Eq, ToHaskell)]
pub(crate) enum LookupCount {
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupKnownCount")]
    Known(i64),
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupUnknownCount")]
    Unknown,
}
impl LookupCount {
    fn known(count: usize) -> Self {
        i64::try_from(count)
            .map(Self::Known)
            .unwrap_or(Self::Unknown)
    }
}
#[derive(ToHaskell)]
#[haskell(module = "Tidepool.Effects.Core", name = "LookupMatchReceipt")]
pub(crate) struct LookupMatchReceipt {
    query: String,
    observed: LookupCount,
    retained: LookupCount,
    limit: Option<i64>,
}
#[derive(ToHaskell)]
pub(crate) enum LookupDiscovery {
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupDiscoveryNotRequested")]
    NotRequested,
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupDiscoveryNotRun")]
    NotRun(LookupSearchNotRun),
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupDiscoveryObserved")]
    Observed(LookupDiscoveryReceipt),
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupDiscoveryInvalidated")]
    Invalidated,
}
#[derive(ToHaskell)]
pub(crate) enum LookupSearchNotRun {
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupSearchStaleView")]
    StaleView,
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupSearchInvalidRequest")]
    InvalidRequest,
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupSearchUnavailable")]
    Unavailable,
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupSearchZeroBudget")]
    ZeroBudget,
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupSearchNoEligibleQuery")]
    NoEligibleQuery,
}
#[derive(ToHaskell)]
#[haskell(module = "Tidepool.Effects.Core", name = "LookupDiscoveryReceipt")]
pub(crate) struct LookupDiscoveryReceipt {
    module_search: LookupModuleSearch,
    attempted_scopes: Vec<LookupSearchScope>,
    eligible_modules: LookupCount,
    submitted_modules: LookupCount,
    module_limit: i64,
    enumerated_occurrences: LookupCount,
    retained_candidates: LookupCount,
    discarded_occurrences: LookupCount,
    distinct_discarded: LookupCount,
    unenumerated_candidates: LookupCount,
    candidate_limit: i64,
    broader_module_count: LookupCount,
    issue: Option<String>,
}
#[derive(ToHaskell)]
pub(crate) enum LookupModuleSearch {
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupModulesNotNeeded")]
    NotNeeded,
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupModulesUnavailable")]
    Unavailable,
    #[haskell(module = "Tidepool.Effects.Core", name = "LookupModulesAttempted")]
    Attempted,
}
fn search_scopes(scopes: &[NameScope]) -> Vec<LookupSearchScope> {
    scopes
        .iter()
        .map(|scope| match scope {
            NameScope::Current => LookupSearchScope::Current,
            NameScope::PublicModule(module) => LookupSearchScope::Module(module.clone()),
        })
        .collect()
}

fn attempted_scopes(results: &[HostedLookupAnswer]) -> Vec<LookupSearchScope> {
    let mut scopes = Vec::new();
    for result in results {
        let observed = match result {
            HostedLookupAnswer::Name { scopes, .. }
            | HostedLookupAnswer::Reference { scopes, .. } => search_scopes(scopes),
            HostedLookupAnswer::Type { .. } => vec![LookupSearchScope::Current],
            HostedLookupAnswer::Rejected { .. } => vec![],
        };
        for scope in observed {
            if !scopes.contains(&scope) {
                scopes.push(scope);
            }
        }
    }
    scopes
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
    pub(crate) contract: Option<String>,
    pub(crate) example: Option<crate::usage_pointer::LookupExample>,
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
            contract: e.contract,
            example: None,
        }
    }
}
impl From<lookup_tool::LookupResult> for LookupResult {
    fn from(r: lookup_tool::LookupResult) -> Self {
        Self {
            query: r.query,
            outcome: match r.outcome {
                lookup_tool::LookupOutcome::Found {
                    matches, truncated, ..
                } => LookupOutcome::Found(matches.into_iter().map(Into::into).collect(), truncated),
                lookup_tool::LookupOutcome::Ambiguous {
                    matches, truncated, ..
                } => LookupOutcome::Ambiguous(
                    matches.into_iter().map(Into::into).collect(),
                    truncated,
                ),
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
        .filter_map(|q| match &q.kind {
            lookup_tool::PreparedLookupKind::Name(name)
            | lookup_tool::PreparedLookupKind::Qualified(name) => {
                let mode = if matches!(q.kind, lookup_tool::PreparedLookupKind::Qualified(_)) {
                    LookupNameMode::NameOrModule
                } else {
                    LookupNameMode::Name
                };
                let qualifier = match mode {
                    LookupNameMode::Name => {
                        lookup_tool::qualifier_and_identifier(name).map(|(qualifier, _)| qualifier)
                    }
                    LookupNameMode::NameOrModule => {
                        name.rsplit_once('.').map(|(qualifier, _)| qualifier)
                    }
                };
                let qualifier_module = qualifier.map(|qualifier| {
                    lookup_tool::resolve_qualifier_module(imports, qualifier)
                        .unwrap_or_else(|| qualifier.into())
                });
                Some(InspectionQuery::LookupName {
                    query: name.clone(),
                    qualifier_module,
                    mode,
                })
            }
            lookup_tool::PreparedLookupKind::Type(query) => {
                Some(InspectionQuery::TypeSearch(query.clone()))
            }
            lookup_tool::PreparedLookupKind::Doc(_)
            | lookup_tool::PreparedLookupKind::Rejected(_) => None,
        })
        .collect()
}

struct LookupInspectionPlan {
    batch: HostedLookupBatch,
}

impl LookupInspectionPlan {
    fn new(
        prepared: &[lookup_tool::PreparedLookup],
        imports: &str,
        references: &[LookupReference],
        discovery: Option<(usize, &[String])>,
    ) -> Result<Self, String> {
        let ordinal = |index| {
            u32::try_from(index).map_err(|_| "lookup ordinal exceeds worker protocol".to_owned())
        };
        let mut operations = Vec::new();
        for (index, prepared) in prepared.iter().enumerate() {
            let Some(query) = queries(std::slice::from_ref(prepared), imports).pop() else {
                continue;
            };
            let query = match query {
                InspectionQuery::LookupName {
                    query,
                    qualifier_module,
                    mode,
                } => HostedLookupQuery::Name(LookupNameInspection {
                    query,
                    qualifier_module,
                    mode,
                }),
                InspectionQuery::TypeSearch(query) => HostedLookupQuery::TypeSearch(query),
                _ => unreachable!("prepared lookup has only name and type operations"),
            };
            operations.push(HostedLookupOperation::Primary {
                ordinal: ordinal(index)?,
                origin: prepared.query.clone(),
                query,
            });
        }
        for (index, reference) in references.iter().enumerate() {
            let query = ExactNameInspection {
                scope: if reference.module.is_empty() {
                    InspectionScope::Current
                } else {
                    InspectionScope::PublicModule(reference.module.clone())
                },
                namespace: match reference.namespace {
                    LookupNamespace::Value => InspectionNamespace::Value,
                    LookupNamespace::Type => InspectionNamespace::Type,
                    LookupNamespace::Constructor => InspectionNamespace::Constructor,
                    LookupNamespace::Field => InspectionNamespace::Field,
                },
                name: reference.name.clone(),
            };
            if let Some(HostedLookupOperation::ExactReference { destinations, .. }) = operations.iter_mut().find(|operation| matches!(operation, HostedLookupOperation::ExactReference { query: existing, .. } if *existing == query)) {
                destinations.push(ordinal(index)?);
            } else {
                operations.push(HostedLookupOperation::ExactReference { destinations: vec![ordinal(index)?], query });
            }
        }
        let discovery = discovery
            .map(|(cap, workspace_modules)| {
                let mut modules = Vec::new();
                for prepared in prepared {
                    if let lookup_tool::PreparedLookupKind::Name(name)
                    | lookup_tool::PreparedLookupKind::Qualified(name) = &prepared.kind
                    {
                        if let Some((qualifier, _)) = name.rsplit_once('.') {
                            let module = lookup_tool::resolve_qualifier_module(imports, qualifier)
                                .unwrap_or_else(|| qualifier.into());
                            if !modules.contains(&module) {
                                modules.push(module);
                            }
                        }
                    }
                }
                let named_count = modules.len();
                for module in workspace_modules {
                    if !modules.contains(module) {
                        modules.push(module.clone());
                    }
                }
                let eligible_modules = ordinal(modules.len())?;
                modules.truncate(128);
                let mut scopes = modules
                    .into_iter()
                    .map(InspectionScope::PublicModule)
                    .collect::<Vec<_>>();
                scopes.insert(named_count.min(scopes.len()), InspectionScope::Current);
                Ok::<_, String>(HostedLookupDiscovery {
                    candidate_limit: cap as u32,
                    scopes,
                    eligible_modules,
                })
            })
            .transpose()?;
        Ok(Self {
            batch: HostedLookupBatch {
                operations,
                discovery,
            },
        })
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

/// Enrich only identities confirmed by the issued source owner. Lookup's
/// declaration module is not evidence of an editable filesystem location.
pub(crate) fn attach_source_locators(
    batch: &mut LookupBatch,
    mut locate: impl FnMut(&str) -> Result<Option<String>, crate::SourceInspectionError>,
) {
    let mut observations = std::collections::BTreeMap::new();
    for result in &mut batch.results {
        let entries = match &mut result.outcome {
            LookupOutcome::Found(entries, _) | LookupOutcome::Ambiguous(entries, _) => entries,
            _ => continue,
        };
        for entry in entries {
            if !matches!(&entry.origin, LookupOrigin::ModuleExport) {
                continue;
            }
            let Some(module) = entry.module.as_deref() else {
                continue;
            };
            let observation = observations
                .entry(module.to_owned())
                .or_insert_with(|| locate(module));
            let detail = match observation {
                Ok(Some(revision)) => format!(
                    "Captured source (requires Source): readSourceModule {module:?}; issued revision {revision}. Draft files may differ."
                ),
                Ok(None) => continue,
                Err(error) => format!("Captured source locator unavailable: {error}"),
            };
            entry.contract = Some(match entry.contract.take() {
                Some(contract) => format!("{contract}\n{detail}"),
                None => detail,
            });
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
    mut inspect: impl FnMut(&HostedLookupBatch) -> Result<HostedLookupOutput, LookupInspectionError>,
) -> LookupBatch {
    let mut batch = LookupBatch {
        search_receipt: LookupSearchReceipt::new(view.clone(), request.discover),
        identity: uuid::Uuid::new_v4().to_string(),
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
        if request.discover {
            batch.search_receipt.discovery = LookupDiscovery::NotRun(LookupSearchNotRun::StaleView);
        }
        batch.issue = Some("lookup compile view changed".into());
        return batch;
    }
    let prepared = if request.queries.is_empty() && !request.references.is_empty() {
        vec![]
    } else {
        match lookup_tool::prepare(serde_json::json!({"queries":request.queries})) {
            Ok(prepared) => prepared,
            Err(error) => {
                if request.discover {
                    batch.search_receipt.discovery =
                        LookupDiscovery::NotRun(LookupSearchNotRun::InvalidRequest);
                }
                batch.issue = Some(error.to_string());
                return batch;
            }
        }
    };
    let cap = request.candidate_limit.clamp(0, 128) as usize;
    let eligible_query = !queries(&prepared, imports).is_empty();
    let discovery = (request.discover && eligible_query).then_some((cap, workspace_modules));
    let plan = match LookupInspectionPlan::new(&prepared, imports, &request.references, discovery) {
        Ok(plan) => plan,
        Err(diagnostic) => {
            batch.issue = Some(diagnostic);
            if request.discover {
                batch.search_receipt.discovery =
                    LookupDiscovery::NotRun(LookupSearchNotRun::InvalidRequest);
            }
            return batch;
        }
    };
    batch.search_receipt.disposition = LookupSearchDisposition::Captured;
    let (output, unavailable, isolated) = if plan.batch.operations.is_empty() {
        (
            HostedLookupOutput {
                answers: vec![],
                discovery: None,
            },
            None,
            false,
        )
    } else {
        match inspect_checked(&plan.batch, &mut inspect) {
            Ok(output) => (output, None, false),
            Err(InspectionFailure::SourceRejected(diagnostic)) => {
                let (answers, unavailable) = isolate(&plan.batch, &mut inspect, &diagnostic);
                (
                    HostedLookupOutput {
                        answers,
                        discovery: None,
                    },
                    unavailable,
                    true,
                )
            }
            Err(InspectionFailure::Unavailable(diagnostic)) => (
                HostedLookupOutput {
                    answers: rejected(&plan.batch.operations, &diagnostic),
                    discovery: None,
                },
                Some(diagnostic),
                false,
            ),
        }
    };
    batch.issue = unavailable;
    batch.search_receipt.attempted_scopes = attempted_scopes(&output.answers);
    if request.discover && !eligible_query {
        batch.search_receipt.discovery = LookupDiscovery::NotRun(if cap == 0 {
            LookupSearchNotRun::ZeroBudget
        } else {
            LookupSearchNotRun::NoEligibleQuery
        });
    } else if let Some(selected) = output.discovery {
        let (module_search, scopes) = match selected.fallback {
            HostedFallback::Attempted(scopes) => {
                (LookupModuleSearch::Attempted, search_scopes(&scopes))
            }
            HostedFallback::Unavailable(diagnostic) => {
                if batch.issue.is_none() {
                    batch.issue = Some(bound_diagnostic(&diagnostic, DIAGNOSTIC_BOUND));
                }
                (LookupModuleSearch::Unavailable, vec![])
            }
            HostedFallback::NotNeeded | HostedFallback::ZeroBudget => {
                (LookupModuleSearch::NotNeeded, vec![])
            }
        };
        batch.candidates = selected
            .candidates
            .into_iter()
            .map(|candidate| LookupCandidate {
                query: candidate.query,
                origins: candidate.origins,
                summary: candidate.summary,
                local: candidate.local,
                reference: candidate.reference.as_ref().map(Into::into),
            })
            .collect();
        batch.search_receipt.discovery = if cap == 0 {
            LookupDiscovery::NotRun(LookupSearchNotRun::ZeroBudget)
        } else {
            LookupDiscovery::Observed(LookupDiscoveryReceipt {
                module_search,
                attempted_scopes: scopes,
                eligible_modules: LookupCount::known(selected.eligible_modules),
                submitted_modules: LookupCount::known(selected.submitted_modules),
                module_limit: 128,
                enumerated_occurrences: LookupCount::known(selected.enumerated_occurrences),
                retained_candidates: LookupCount::known(batch.candidates.len()),
                discarded_occurrences: LookupCount::known(selected.discarded_occurrences),
                distinct_discarded: if selected.discarded_occurrences == 0 {
                    LookupCount::Known(0)
                } else {
                    LookupCount::Unknown
                },
                unenumerated_candidates: LookupCount::Unknown,
                candidate_limit: cap as i64,
                broader_module_count: LookupCount::Unknown,
                issue: selected.issue.or_else(|| batch.issue.clone()),
            })
        };
    } else if request.discover && (isolated || batch.issue.is_some()) {
        // Isolation may recover primary facts; it never replays fallback search.
        batch.search_receipt.discovery = LookupDiscovery::NotRun(LookupSearchNotRun::Unavailable);
    }
    let primary_len = prepared.len();
    let mut normalizer_inputs = std::collections::BTreeMap::new();
    for answer in output.answers {
        match &answer {
            HostedLookupAnswer::Name { ordinal, .. }
            | HostedLookupAnswer::Type { ordinal, .. }
            | HostedLookupAnswer::Rejected {
                operation: HostedOperationIdentity::Primary(ordinal),
                ..
            } => {
                normalizer_inputs.insert(*ordinal as usize, answer);
            }
            HostedLookupAnswer::Reference { destinations, .. }
            | HostedLookupAnswer::Rejected {
                operation: HostedOperationIdentity::Reference(destinations),
                ..
            } => {
                // Repeated authored references share one physical query. Only
                // their already-bounded selected result needs repeated delivery.
                let destinations = destinations.clone();
                let mut answer = Some(answer);
                for (index, destination) in destinations.iter().enumerate() {
                    let delivered = if index + 1 == destinations.len() {
                        answer.take().expect("last reference delivery")
                    } else {
                        answer.as_ref().expect("reference delivery").clone()
                    };
                    normalizer_inputs.insert(primary_len + *destination as usize, delivered);
                }
            }
        }
    }
    let mut prepared = prepared;
    prepared.extend(
        request
            .references
            .into_iter()
            .map(|reference| lookup_tool::PreparedLookup {
                query: if reference.module.is_empty() {
                    reference.name.clone()
                } else {
                    format!("{}.{}", reference.module, reference.name)
                },
                kind: lookup_tool::PreparedLookupKind::Name(reference.name),
            }),
    );
    let response = lookup_tool::resolve(
        prepared,
        normalizer_inputs,
        live_modules,
        workspace_modules,
        usage.clone(),
    );
    batch.search_receipt.matches = response
        .results
        .iter()
        .map(|result| {
            let (observed, retained, limit) = match &result.outcome {
                lookup_tool::LookupOutcome::Found {
                    matches,
                    total_matches,
                    match_limit,
                    ..
                }
                | lookup_tool::LookupOutcome::Ambiguous {
                    matches,
                    total_matches,
                    match_limit,
                    ..
                } => (
                    LookupCount::known(*total_matches),
                    LookupCount::known(matches.len()),
                    i64::try_from(*match_limit).ok(),
                ),
                lookup_tool::LookupOutcome::NotFound { .. } => {
                    (LookupCount::Known(0), LookupCount::Known(0), None)
                }
                lookup_tool::LookupOutcome::Rejected { .. } => {
                    (LookupCount::Unknown, LookupCount::Known(0), None)
                }
            };
            LookupMatchReceipt {
                query: result.query.clone(),
                observed,
                retained,
                limit,
            }
        })
        .collect();
    batch.results = response.results.into_iter().map(Into::into).collect();
    // Direct queries and exact references share the resolved defining identity.
    // Never attach owner examples to a shadow, ambiguous hit or truncated set.
    for result in &mut batch.results {
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

fn rejected(operations: &[HostedLookupOperation], diagnostic: &str) -> Vec<HostedLookupAnswer> {
    let diagnostic = bound_diagnostic(diagnostic, DIAGNOSTIC_BOUND);
    operations
        .iter()
        .map(|operation| HostedLookupAnswer::Rejected {
            operation: match operation {
                HostedLookupOperation::Primary { ordinal, .. } => {
                    HostedOperationIdentity::Primary(*ordinal)
                }
                HostedLookupOperation::ExactReference { destinations, .. } => {
                    HostedOperationIdentity::Reference(destinations.clone())
                }
            },
            diagnostic: diagnostic.clone(),
        })
        .collect()
}

fn inspect_checked(
    batch: &HostedLookupBatch,
    inspect: &mut impl FnMut(&HostedLookupBatch) -> Result<HostedLookupOutput, LookupInspectionError>,
) -> Result<HostedLookupOutput, InspectionFailure> {
    let result = inspect(batch).and_then(|output| {
        output
            .validate_for(batch)
            .map_err(LookupInspectionError::Compiler)?;
        Ok(output)
    });
    match result {
        Ok(output) => Ok(output),
        Err(error) => {
            let diagnostic =
                bound_diagnostic(&render_lookup_inspection_error(&error), DIAGNOSTIC_BOUND);
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
    batch: &HostedLookupBatch,
    inspect: &mut impl FnMut(&HostedLookupBatch) -> Result<HostedLookupOutput, LookupInspectionError>,
    original_diagnostic: &str,
) -> (Vec<HostedLookupAnswer>, Option<String>) {
    if batch.operations.len() == 1 {
        return (rejected(&batch.operations, original_diagnostic), None);
    }
    let mut results = Vec::new();
    let mut unavailable: Option<String> = None;
    for operation in &batch.operations {
        let operations = std::slice::from_ref(operation);
        if let Some(diagnostic) = &unavailable {
            results.extend(rejected(operations, diagnostic));
            continue;
        }
        let shard = HostedLookupBatch {
            operations: operations.to_vec(),
            discovery: None,
        };
        match inspect_checked(&shard, inspect) {
            Ok(output) => results.extend(output.answers),
            Err(InspectionFailure::SourceRejected(diagnostic)) => {
                results.extend(rejected(operations, &diagnostic))
            }
            Err(InspectionFailure::Unavailable(diagnostic)) => {
                results.extend(rejected(operations, &diagnostic));
                unavailable = Some(diagnostic);
            }
        }
    }
    (results, unavailable)
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
    fn captured_source_routes_use_owner_identity_and_preserve_absence_and_failure() {
        let make_entry = |module: &str| LookupEntry {
            name: "work".into(),
            module: Some(module.into()),
            declaration: "work :: Int".into(),
            kind: LookupKind::Value,
            availability: LookupAvailability::Available,
            origin: LookupOrigin::ModuleExport,
            quality: LookupQuality::Exact,
            usage: None,
            contract: Some("Original contract".into()),
            example: None,
        };
        let mut batch = LookupBatch {
            identity: "capture".into(),
            view: "view".into(),
            issue: None,
            candidates: vec![],
            search_receipt: LookupSearchReceipt::new("view".into(), false),
            results: vec![LookupResult {
                query: "work".into(),
                outcome: LookupOutcome::Found(
                    vec![
                        make_entry("Captured.Work"),
                        make_entry("Captured.Work"),
                        make_entry("Installed.Library"),
                        make_entry("Broken.Source"),
                    ],
                    false,
                ),
            }],
        };
        let mut observed = Vec::new();
        attach_source_locators(&mut batch, |module| {
            observed.push(module.to_owned());
            match module {
                "Captured.Work" => Ok(Some("issued-original".into())),
                "Installed.Library" => Ok(None),
                _ => Err(crate::SourceInspectionError::Unavailable(
                    "owner refused".into(),
                )),
            }
        });
        assert_eq!(
            observed,
            ["Captured.Work", "Installed.Library", "Broken.Source"]
        );
        let LookupOutcome::Found(entries, _) = &batch.results[0].outcome else {
            panic!("found")
        };
        assert!(entries[0]
            .contract
            .as_ref()
            .unwrap()
            .contains("readSourceModule \"Captured.Work\"; issued revision issued-original"));
        assert_eq!(entries[0].contract, entries[1].contract);
        assert_eq!(entries[2].contract.as_deref(), Some("Original contract"));
        assert!(entries[3]
            .contract
            .as_ref()
            .unwrap()
            .contains("locator unavailable: source unavailable: owner refused"));
    }

    #[test]
    fn semantic_lookup_keeps_existing_qualifier_recognition() {
        let prepared = lookup_tool::prepare(serde_json::json!({"queries": ["value", "lower.value", "Alias.value", "Project.Choice"]})).unwrap();
        let planned = queries(&prepared, "qualified Project.Work as Alias");
        assert!(matches!(
            &planned[0],
            InspectionQuery::LookupName {
                qualifier_module: None,
                mode: LookupNameMode::Name,
                ..
            }
        ));
        assert!(matches!(
            &planned[1],
            InspectionQuery::LookupName {
                qualifier_module: None,
                mode: LookupNameMode::Name,
                ..
            }
        ));
        assert!(
            matches!(&planned[2], InspectionQuery::LookupName { qualifier_module: Some(module), mode: LookupNameMode::Name, .. } if module == "Project.Work")
        );
        assert!(
            matches!(&planned[3], InspectionQuery::LookupName { qualifier_module: Some(module), mode: LookupNameMode::NameOrModule, .. } if module == "Project")
        );
    }

    #[test]
    fn search_receipt_uses_worker_attempts_instead_of_optional_query_paths() {
        for scopes in [
            vec![NameScope::Current],
            vec![
                NameScope::Current,
                NameScope::PublicModule("Project".into()),
            ],
            vec![],
        ] {
            let issued = scopes.clone();
            let batch = execute(
                request(&["Alias.value"], false),
                "view".into(),
                "qualified Project as Alias",
                &[],
                &[],
                crate::UsagePointerTable::default(),
                |queries| {
                    assert!(
                        matches!(&queries[..], [InspectionQuery::LookupName { qualifier_module: Some(module), .. }] if module == "Project")
                    );
                    Ok(vec![InspectionResult::Lookup {
                        query: "Alias.value".into(),
                        scopes: issued.clone(),
                        answer: LookupAnswer::Missing {
                            export_names: vec![],
                        },
                    }])
                },
            );
            assert_eq!(batch.search_receipt.attempted_scopes.len(), scopes.len());
            for (observed, scope) in batch.search_receipt.attempted_scopes.iter().zip(&scopes) {
                assert!(matches!(
                    (observed, scope),
                    (LookupSearchScope::Current, NameScope::Current)
                        | (LookupSearchScope::Module(_), NameScope::PublicModule(_))
                ));
            }
        }
        let batch = execute(
            request(&["Alias.value"], false),
            "view".into(),
            "qualified Project as Alias",
            &[],
            &[],
            crate::UsagePointerTable::default(),
            |_| Err(source_error("query rejected before name search")),
        );
        assert!(batch.search_receipt.attempted_scopes.is_empty());
        assert!(matches!(
            batch.results[0].outcome,
            LookupOutcome::Rejected(_)
        ));
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
                        InspectionQuery::LookupName {
                            query,
                            mode: LookupNameMode::NameOrModule,
                            ..
                        } => {
                            let answer = match query.as_str() {
                                "Project.Work" => LookupAnswer::Module {
                                    module: query.clone(),
                                    entries: vec![entry("runWork")],
                                },
                                "Project.Choice" => {
                                    let mut ty = entry("Choice");
                                    ty.kind = "type".into();
                                    let mut constructor = entry("Choice");
                                    constructor.kind = "constructor".into();
                                    LookupAnswer::Ambiguous(vec![ty, constructor])
                                }
                                _ => panic!("unexpected semantic query {query}"),
                            };
                            InspectionResult::Lookup {
                                scopes: vec![NameScope::Current],
                                query: query.clone(),
                                answer,
                            }
                        }
                        InspectionQuery::ExactName(ExactNameQuery {
                            namespace: IdentifierNamespace::Constructor,
                            name,
                            ..
                        }) => {
                            let mut constructor = entry(name);
                            constructor.kind = "constructor".into();
                            InspectionResult::Lookup {
                                scopes: vec![NameScope::PublicModule("Project".into())],
                                query: name.clone(),
                                answer: LookupAnswer::Info(vec![constructor]),
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
                        InspectionQuery::LookupName { query, .. } if query == "x" => {
                            Ok(InspectionResult::Lookup {
                                scopes: vec![NameScope::Current],
                                query: query.clone(),
                                answer: LookupAnswer::Info(vec![entry("x")]),
                            })
                        }
                        InspectionQuery::LookupName { query, .. } if query == "Project.Work" => {
                            Ok(InspectionResult::Lookup {
                                scopes: vec![NameScope::Current],
                                query: query.clone(),
                                answer: LookupAnswer::Module {
                                    module: query.clone(),
                                    entries: vec![entry("runWork")],
                                },
                            })
                        }
                        InspectionQuery::ExactName(_) => {
                            Err(source_error("reference source rejected"))
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
                Ok(vec![InspectionResult::Lookup {
                    scopes: vec![NameScope::Current],
                    query: "x".into(),
                    answer: LookupAnswer::Info(vec![entry("x")]),
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
        let mut direct_and_exact = request(&["Cmd.start", "Alias.start"], false);
        direct_and_exact.expected_view = Some("view".into());
        direct_and_exact.references = vec![LookupReference {
            module: "Reexport.Command".into(),
            name: "start".into(),
            namespace: LookupNamespace::Value,
        }];
        let found = execute(
            direct_and_exact,
            "view".into(),
            "qualified Tidepool.Command as Cmd\nqualified Tidepool.Command as Alias",
            &[],
            &[],
            usage.clone(),
            |queries| {
                Ok(queries
                    .iter()
                    .map(|query| match query {
                        InspectionQuery::LookupName { query: name, .. } => {
                            InspectionResult::Lookup {
                                scopes: vec![NameScope::Current],
                                query: name.clone(),
                                answer: LookupAnswer::Info(vec![make_entry(
                                    "Tidepool.Command",
                                    available,
                                )]),
                            }
                        }
                        InspectionQuery::ExactName(query) => InspectionResult::Lookup {
                            scopes: vec![query.scope.clone()],
                            query: query.name.clone(),
                            answer: LookupAnswer::Info(vec![make_entry(
                                "Tidepool.Command",
                                available,
                            )]),
                        },
                        _ => panic!("unexpected query"),
                    })
                    .collect())
            },
        );
        assert_eq!(found.results.len(), 3);
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
                Ok(vec![InspectionResult::Lookup {
                    scopes: vec![NameScope::Current],
                    query: "start".into(),
                    answer: LookupAnswer::Info(vec![make_entry("Other.Command", available)]),
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
                Ok(vec![InspectionResult::Lookup {
                    scopes: vec![NameScope::Current],
                    query: "start".into(),
                    answer: LookupAnswer::Info(vec![make_entry("Tidepool.Command", available)]),
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
                Ok(vec![InspectionResult::Lookup {
                    scopes: vec![NameScope::Current],
                    query: "start".into(),
                    answer: LookupAnswer::Info(vec![make_entry(
                        "Tidepool.Command",
                        InspectionAvailability::Unavailable,
                    )]),
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
            usage.clone(),
            |_| {
                Ok(vec![InspectionResult::Lookup {
                    scopes: vec![NameScope::Current],
                    query: "start".into(),
                    answer: LookupAnswer::Ambiguous(vec![
                        make_entry("Tidepool.Command", available),
                        make_entry("Other.Command", available),
                    ]),
                }])
            },
        );
        assert!(matches!(
            ambiguous.results[0].outcome,
            LookupOutcome::Ambiguous(_, _)
        ));
        for (live_modules, namespace, entries) in [
            (
                vec!["Tidepool.Command".into()],
                LookupNamespace::Value,
                vec![make_entry("Tidepool.Command", available)],
            ),
            (
                vec![],
                LookupNamespace::Type,
                vec![make_entry("Tidepool.Command", available)],
            ),
            (
                vec![],
                LookupNamespace::Value,
                vec![make_entry(
                    "Tidepool.Command",
                    InspectionAvailability::Unavailable,
                )],
            ),
            (
                vec![],
                LookupNamespace::Value,
                vec![
                    make_entry("Tidepool.Command", available),
                    make_entry("Other.Command", available),
                ],
            ),
        ] {
            let expected_missing = namespace == LookupNamespace::Type;
            let expected_ambiguous = entries.len() > 1;
            let mut exact = request(&[], false);
            exact.expected_view = Some("view".into());
            exact.references = vec![LookupReference {
                module: "Reexport.Command".into(),
                name: "start".into(),
                namespace,
            }];
            let captured = execute(
                exact,
                "view".into(),
                "",
                &live_modules,
                &[],
                usage.clone(),
                |queries| {
                    Ok(queries
                        .iter()
                        .map(|query| {
                            let InspectionQuery::ExactName(query) = query else {
                                panic!()
                            };
                            assert_eq!(query.name, "start");
                            InspectionResult::Lookup {
                                query: query.name.clone(),
                                scopes: vec![query.scope.clone()],
                                answer: if expected_missing {
                                    LookupAnswer::Missing {
                                        export_names: vec![],
                                    }
                                } else if expected_ambiguous {
                                    LookupAnswer::Ambiguous(entries.clone())
                                } else {
                                    LookupAnswer::Info(entries.clone())
                                },
                            }
                        })
                        .collect())
                },
            );
            match &captured.results[0].outcome {
                LookupOutcome::Found(entries, false)
                    if !expected_missing && !expected_ambiguous =>
                {
                    assert_eq!(entries.len(), 1);
                    assert!(entries[0].example.is_none());
                }
                LookupOutcome::Ambiguous(entries, false) if expected_ambiguous => {
                    assert_eq!(entries.len(), 2);
                    assert!(entries.iter().all(|entry| entry.example.is_none()));
                }
                LookupOutcome::Missing(_, _) if expected_missing => {}
                _ => panic!("unexpected exact-reference outcome"),
            }
        }
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
                        let InspectionQuery::LookupName { query: name, .. } = q else {
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
                        InspectionResult::Lookup {
                            scopes: vec![NameScope::Current],
                            query: name.clone(),
                            answer: LookupAnswer::Info(vec![e]),
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
                        let InspectionQuery::LookupName { query: name, .. } = query else {
                            panic!()
                        };
                        let mut entry = entry(name);
                        entry.references = vec![IdentifierRef {
                            module: "Project.Test".into(),
                            name: "Shared".into(),
                            namespace: IdentifierNamespace::Type,
                        }];
                        InspectionResult::Lookup {
                            scopes: vec![NameScope::Current],
                            query: name.clone(),
                            answer: LookupAnswer::Info(vec![entry]),
                        }
                    })
                    .collect())
            },
        );
        assert_eq!(result.candidates.len(), 1);
        assert_eq!(result.candidates[0].origins, vec!["a", "b"]);
    }
    #[test]
    fn search_receipt_records_match_cap_before_discard() {
        for count in [0, 19, 20, 21, 40] {
            let captured = execute(
                request(&["value"], false),
                "view".into(),
                "",
                &[],
                &[],
                crate::UsagePointerTable::default(),
                |_| {
                    Ok(vec![InspectionResult::Lookup {
                        scopes: vec![NameScope::Current],
                        query: "value".into(),
                        answer: LookupAnswer::Info((0..count).map(|_| entry("value")).collect()),
                    }])
                },
            );
            let receipt = &captured.search_receipt.matches[0];
            assert_eq!(receipt.observed, LookupCount::known(count));
            assert_eq!(receipt.retained, LookupCount::known(count.min(20)));
            assert_eq!(receipt.limit, Some(20));
            assert!(matches!(
                captured.search_receipt.discovery,
                LookupDiscovery::NotRequested
            ));
        }
    }
    #[test]
    fn search_receipt_records_effective_candidate_cap_boundaries() {
        for requested in [-1, 0, 1, 127, 128, 129] {
            let mut req = request(&["root"], true);
            req.candidate_limit = requested;
            let captured = execute(
                req,
                "view".into(),
                "",
                &[],
                &[],
                crate::UsagePointerTable::default(),
                |_| {
                    let mut hit = entry("root");
                    hit.references = (0..129)
                        .map(|n| IdentifierRef {
                            module: "Project.Other".into(),
                            name: format!("Ref{n}"),
                            namespace: IdentifierNamespace::Type,
                        })
                        .collect();
                    Ok(vec![InspectionResult::Lookup {
                        scopes: vec![NameScope::Current],
                        query: "root".into(),
                        answer: LookupAnswer::Info(vec![hit]),
                    }])
                },
            );
            let cap = requested.clamp(0, 128);
            if cap == 0 {
                assert!(matches!(
                    captured.search_receipt.discovery,
                    LookupDiscovery::NotRun(LookupSearchNotRun::ZeroBudget)
                ));
            } else {
                let LookupDiscovery::Observed(receipt) = captured.search_receipt.discovery else {
                    panic!()
                };
                assert_eq!(receipt.candidate_limit, cap);
                assert_eq!(receipt.enumerated_occurrences, LookupCount::Known(129));
                assert_eq!(receipt.retained_candidates, LookupCount::Known(cap));
                assert_eq!(receipt.discarded_occurrences, LookupCount::Known(129 - cap));
                assert_eq!(receipt.distinct_discarded, LookupCount::Unknown);
                assert_eq!(receipt.unenumerated_candidates, LookupCount::Unknown);
            }
        }
    }
    #[test]
    fn search_receipt_distinguishes_module_cap_from_unsearched_universe() {
        let modules: Vec<_> = (0..130).map(|n| format!("Project.Module{n}")).collect();
        let captured = execute(
            request(&["absent"], true),
            "view".into(),
            "",
            &[],
            &modules,
            crate::UsagePointerTable::default(),
            |queries| {
                Ok(queries
                    .iter()
                    .map(|query| match query {
                        InspectionQuery::LookupName { query: name, .. } => {
                            InspectionResult::Lookup {
                                scopes: vec![NameScope::Current],
                                query: name.clone(),
                                answer: LookupAnswer::Missing {
                                    export_names: vec![],
                                },
                            }
                        }
                        InspectionQuery::ScopeBrowse => InspectionResult::Browse {
                            module: "".into(),
                            expanded: true,
                            entries: vec![],
                        },
                        InspectionQuery::Browse { module, expanded } => InspectionResult::Browse {
                            module: module.clone(),
                            expanded: *expanded,
                            entries: vec![],
                        },
                        _ => panic!(),
                    })
                    .collect())
            },
        );
        let LookupDiscovery::Observed(receipt) = captured.search_receipt.discovery else {
            panic!()
        };
        assert_eq!(receipt.eligible_modules, LookupCount::Known(130));
        assert_eq!(receipt.submitted_modules, LookupCount::Known(128));
        assert_eq!(receipt.attempted_scopes.len(), 129);
        assert!(matches!(
            receipt.module_search,
            LookupModuleSearch::Attempted
        ));
        assert_eq!(receipt.broader_module_count, LookupCount::Unknown);
        assert_eq!(receipt.enumerated_occurrences, LookupCount::Known(0));
    }
    #[test]
    fn discovery_submission_is_distinct_from_scopes_rejected_before_search() {
        let captured = execute(
            request(&["Missing.value"], true),
            "view".into(),
            "",
            &[],
            &[],
            crate::UsagePointerTable::default(),
            |queries| {
                if matches!(&queries[..], [InspectionQuery::LookupName { .. }]) {
                    Ok(vec![InspectionResult::Lookup {
                        query: "Missing.value".into(),
                        scopes: vec![
                            NameScope::Current,
                            NameScope::PublicModule("Missing".into()),
                        ],
                        answer: LookupAnswer::Missing {
                            export_names: vec![],
                        },
                    }])
                } else {
                    assert_eq!(
                        queries
                            .iter()
                            .filter(|query| matches!(query, InspectionQuery::Browse { .. }))
                            .count(),
                        1
                    );
                    Err(source_error("discovery source rejected before name search"))
                }
            },
        );
        let LookupDiscovery::Observed(receipt) = captured.search_receipt.discovery else {
            panic!()
        };
        assert_eq!(receipt.submitted_modules, LookupCount::Known(1));
        assert!(receipt.attempted_scopes.is_empty());
        assert!(receipt.issue.is_some());
    }

    #[test]
    fn search_receipt_does_not_turn_not_run_into_zero_coverage() {
        let mut stale = request(&["value"], true);
        stale.expected_view = Some("old-view".into());
        let captured = execute(
            stale,
            "new-view".into(),
            "",
            &[],
            &[],
            crate::UsagePointerTable::default(),
            |_| panic!("stale view must not inspect"),
        );
        assert_eq!(captured.search_receipt.view, "new-view");
        assert!(captured.search_receipt.matches.is_empty());
        assert!(captured.search_receipt.attempted_scopes.is_empty());
        assert!(matches!(
            captured.search_receipt.disposition,
            LookupSearchDisposition::NotExecuted
        ));
        assert!(matches!(
            captured.search_receipt.discovery,
            LookupDiscovery::NotRun(LookupSearchNotRun::StaleView)
        ));
        let docs = execute(
            request(&["doc workbench"], true),
            "view".into(),
            "",
            &[],
            &[],
            crate::UsagePointerTable::default(),
            |_| panic!("docs do not inspect"),
        );
        assert!(matches!(
            docs.search_receipt.discovery,
            LookupDiscovery::NotRun(LookupSearchNotRun::NoEligibleQuery)
        ));
        assert!(docs.search_receipt.attempted_scopes.is_empty());
        let mut zero = request(&["value"], true);
        zero.candidate_limit = 0;
        let captured = execute(
            zero,
            "view".into(),
            "",
            &[],
            &[],
            crate::UsagePointerTable::default(),
            |_| {
                Ok(vec![InspectionResult::Lookup {
                    scopes: vec![NameScope::Current],
                    query: "value".into(),
                    answer: LookupAnswer::Info(vec![entry("value")]),
                }])
            },
        );
        assert!(matches!(
            captured.search_receipt.discovery,
            LookupDiscovery::NotRun(LookupSearchNotRun::ZeroBudget)
        ));
        assert!(captured.candidates.is_empty());
    }
    #[test]
    fn search_receipt_invalidates_executed_facts_without_fabricating_zero_counts() {
        let mut captured = execute(
            request(&["value"], true),
            "view".into(),
            "",
            &[],
            &[],
            crate::UsagePointerTable::default(),
            |_| {
                Ok(vec![InspectionResult::Lookup {
                    scopes: vec![NameScope::Current],
                    query: "value".into(),
                    answer: LookupAnswer::Info(vec![entry("value")]),
                }])
            },
        );
        assert_eq!(
            captured.search_receipt.matches[0].observed,
            LookupCount::Known(1)
        );
        assert!(matches!(
            captured.search_receipt.discovery,
            LookupDiscovery::Observed(_)
        ));
        captured.invalidate_changed_view();
        captured.invalidate_changed_view();
        assert!(captured.results.is_empty());
        assert!(captured.candidates.is_empty());
        assert!(captured.search_receipt.matches.is_empty());
        assert_eq!(
            captured.search_receipt.attempted_scopes,
            [LookupSearchScope::Current]
        );
        let LookupSearchDisposition::Invalidated(work, discovery) =
            captured.search_receipt.disposition
        else {
            panic!()
        };
        assert_eq!(work.len(), 1);
        assert_eq!(work[0].observed, LookupCount::Known(1));
        assert!(matches!(discovery, LookupDiscovery::Observed(_)));
        assert!(matches!(
            captured.search_receipt.discovery,
            LookupDiscovery::Invalidated
        ));
    }
    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(64))]
        #[test]
        fn search_receipt_duplicate_histories_count_only_discarded_occurrences(width in 1usize..9, cap in 1usize..12) {
            let mut req = request(&["a", "b"], true);
            req.candidate_limit = cap as i64;
            let captured = execute(req, "view".into(), "", &[], &[], crate::UsagePointerTable::default(), |queries| {
                Ok(queries.iter().map(|query| {
                    let InspectionQuery::LookupName { query: name, .. } = query else { panic!() };
                    let mut hit = entry(name);
                    hit.references = (0..width).chain(std::iter::once(0)).map(|n| IdentifierRef {
                        module: "Project.Other".into(), name: format!("Ref{n}"), namespace: IdentifierNamespace::Type,
                    }).collect();
                    InspectionResult::Lookup { scopes: vec![NameScope::Current], query: name.clone(), answer: LookupAnswer::Info(vec![hit]) }
                }).collect())
            });
            let LookupDiscovery::Observed(receipt) = &captured.search_receipt.discovery else { panic!() };
            let retained = width.min(cap);
            proptest::prop_assert_eq!(&receipt.enumerated_occurrences, &LookupCount::known(2 * (width + 1)));
            proptest::prop_assert_eq!(&receipt.retained_candidates, &LookupCount::known(retained));
            proptest::prop_assert_eq!(&receipt.discarded_occurrences, &LookupCount::known(2 * (width - retained)));
            proptest::prop_assert_eq!(&receipt.distinct_discarded, &if width == retained { LookupCount::Known(0) } else { LookupCount::Unknown });
            proptest::prop_assert!(matches!(receipt.module_search, LookupModuleSearch::NotNeeded));
            proptest::prop_assert!(receipt.attempted_scopes.is_empty());
            proptest::prop_assert!(captured.candidates.iter().all(|candidate| candidate.origins == ["a", "b"]));
        }
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
                        InspectionQuery::LookupName { query: name, .. } => {
                            InspectionResult::Lookup {
                                scopes: vec![NameScope::Current],
                                query: name.clone(),
                                answer: LookupAnswer::Missing {
                                    export_names: vec![],
                                },
                            }
                        }
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
                    InspectionQuery::LookupName { query: name, .. } if name == "validName" => {
                        Ok(vec![InspectionResult::Lookup {
                            scopes: vec![NameScope::Current],
                            query: name.clone(),
                            answer: LookupAnswer::Info(vec![entry("validName")]),
                        }])
                    }
                    InspectionQuery::LookupName { query: name, .. } if name == "unknownName" => {
                        Ok(vec![InspectionResult::Lookup {
                            scopes: vec![NameScope::Current],
                            query: name.clone(),
                            answer: LookupAnswer::Missing {
                                export_names: vec![],
                            },
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
            search_receipt: LookupSearchReceipt::new("view".into(), false),
            identity: uuid::Uuid::new_v4().to_string(),
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
            search_receipt: LookupSearchReceipt::new("view".into(), false),
            identity: uuid::Uuid::new_v4().to_string(),
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
                        assert!(matches!(
                            q,
                            InspectionQuery::ExactName(ExactNameQuery {
                                namespace: IdentifierNamespace::Constructor,
                                ..
                            })
                        ));
                        InspectionResult::Lookup {
                            scopes: vec![NameScope::PublicModule("Project.Test".into())],
                            query: "Same".into(),
                            answer: LookupAnswer::Info(vec![InfoEntry {
                                name: "Same".into(),
                                module: Some("Project.Test".into()),
                                kind: "constructor".into(),
                                display: "constructor".into(),
                                availability: InspectionAvailability::Available,
                                references: vec![],
                            }]),
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
