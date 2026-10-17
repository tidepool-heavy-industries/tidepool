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

    use tidepool_runtime::session::inspection::{
        HostedDiscoveryCandidate, HostedDiscoverySelection, SelectedLookupAnswer, SelectedMatches,
    };

    fn selected(entries: Vec<InfoEntry>) -> SelectedLookupAnswer {
        SelectedLookupAnswer::Info(SelectedMatches {
            observed: entries.len(),
            entries,
            limit: 20,
        })
    }
    fn name(ordinal: u32, answer: SelectedLookupAnswer) -> HostedLookupAnswer {
        HostedLookupAnswer::Name {
            ordinal,
            scopes: vec![NameScope::Current],
            answer,
        }
    }
    fn output(answers: Vec<HostedLookupAnswer>) -> HostedLookupOutput {
        HostedLookupOutput {
            answers,
            discovery: None,
        }
    }
    fn selection(
        batch: &HostedLookupBatch,
        fallback: HostedFallback,
        candidates: Vec<HostedDiscoveryCandidate>,
        enumerated: usize,
        discarded: usize,
    ) -> HostedDiscoverySelection {
        let policy = batch.discovery.as_ref().expect("discovery policy");
        HostedDiscoverySelection {
            submitted_modules: if matches!(fallback, HostedFallback::Attempted(_)) {
                policy
                    .scopes
                    .iter()
                    .filter(|scope| matches!(scope, InspectionScope::PublicModule(_)))
                    .count()
            } else {
                0
            },
            candidates,
            fallback,
            eligible_modules: policy.eligible_modules as usize,
            candidate_limit: policy.candidate_limit as usize,
            enumerated_occurrences: enumerated,
            discarded_occurrences: discarded,
            issue: None,
        }
    }

    #[test]
    fn hosted_plan_preserves_original_ordinals_and_shared_reference_destinations() {
        let prepared = lookup_tool::prepare(
            serde_json::json!({"queries":["doc topics", "value", "", "value", ":: Int -> Int"]}),
        )
        .unwrap();
        let reference = LookupReference {
            module: String::new(),
            name: "Defining.Owner.value".into(),
            namespace: LookupNamespace::Value,
        };
        let plan = LookupInspectionPlan::new(
            &prepared,
            "",
            &[reference.clone(), reference],
            Some((20, &[])),
        )
        .unwrap();
        assert!(matches!(&plan.batch.operations[..], [
            HostedLookupOperation::Primary { ordinal: 1, origin: first, .. },
            HostedLookupOperation::Primary { ordinal: 3, origin: second, .. },
            HostedLookupOperation::Primary { ordinal: 4, query: HostedLookupQuery::TypeSearch(_), .. },
            HostedLookupOperation::ExactReference { destinations, query: ExactNameInspection { scope: InspectionScope::Current, name, .. } },
        ] if first == "value" && second == "value" && destinations == &[0,1] && name == "Defining.Owner.value"));
        plan.batch.validate().unwrap();
    }

    #[test]
    fn search_receipt_uses_worker_attempts_instead_of_optional_query_paths() {
        for scopes in [
            vec![NameScope::Current],
            vec![
                NameScope::Current,
                NameScope::PublicModule("Project".into()),
            ],
        ] {
            let result = execute(
                request(&["Alias.value"], false),
                "view".into(),
                "qualified Project as Alias",
                &[],
                &[],
                Default::default(),
                |batch| {
                    assert!(
                        matches!(&batch.operations[..], [HostedLookupOperation::Primary { query: HostedLookupQuery::Name(LookupNameInspection { qualifier_module: Some(owner), .. }), .. }] if owner == "Project")
                    );
                    Ok(output(vec![HostedLookupAnswer::Name {
                        ordinal: 0,
                        scopes: scopes.clone(),
                        answer: SelectedLookupAnswer::Missing {
                            export_names: vec![],
                        },
                    }]))
                },
            );
            assert_eq!(
                result.search_receipt.attempted_scopes,
                search_scopes(&scopes)
            );
        }
    }

    #[test]
    fn hosted_results_preserve_counts_order_and_reference_namespace_without_ranking() {
        let mut req = request(&["doc topics", "Project.Value", "value"], false);
        let reference = LookupReference {
            module: "Project".into(),
            name: "Choice".into(),
            namespace: LookupNamespace::Constructor,
        };
        req.references = vec![reference.clone(), reference];
        let calls = Cell::new(0);
        let result = execute(
            req,
            "view".into(),
            "",
            &[],
            &[],
            Default::default(),
            |batch| {
                calls.set(calls.get() + 1);
                assert!(batch.discovery.is_none());
                assert!(
                    matches!(&batch.operations[2], HostedLookupOperation::ExactReference { destinations, query: ExactNameInspection { namespace: InspectionNamespace::Constructor, .. } } if destinations == &[0,1])
                );
                let mut constructor = entry("Choice");
                constructor.kind = "constructor".into();
                // Deliberately reverse lexical order. The host must not normalize
                // a selected prefix again or infer its observed count from length.
                let entries = (0..20)
                    .rev()
                    .map(|i| entry(&format!("value{i:02}")))
                    .collect();
                Ok(output(vec![
                    HostedLookupAnswer::Name {
                        ordinal: 1,
                        scopes: vec![
                            NameScope::Current,
                            NameScope::PublicModule("Project.Value".into()),
                        ],
                        answer: SelectedLookupAnswer::Module {
                            module: "Project.Value".into(),
                            matches: SelectedMatches {
                                entries,
                                observed: 99,
                                limit: 20,
                            },
                        },
                    },
                    name(2, selected(vec![entry("second")])),
                    HostedLookupAnswer::Reference {
                        destinations: vec![0, 1],
                        scopes: vec![NameScope::PublicModule("Project".into())],
                        answer: selected(vec![constructor]),
                    },
                ]))
            },
        );
        assert_eq!(calls.get(), 1);
        assert_eq!(result.results.len(), 5);
        let LookupOutcome::Found(entries, true) = &result.results[1].outcome else {
            panic!("selected module prefix")
        };
        assert_eq!(entries[0].name, "value19");
        assert_eq!(
            result.search_receipt.matches[1].observed,
            LookupCount::Known(99)
        );
        assert_eq!(
            result.search_receipt.matches[1].retained,
            LookupCount::Known(20)
        );
        for index in [3, 4] {
            assert!(
                matches!(&result.results[index].outcome, LookupOutcome::Found(entries, false) if matches!(entries[0].kind, LookupKind::Constructor))
            );
        }
    }

    #[test]
    fn discovery_is_issued_with_the_primary_request_and_keeps_complete_selected_summary() {
        let calls = Cell::new(0);
        let summary = "λ complete selected declaration\n".repeat(300);
        let modules = (0..130)
            .map(|i| format!("Workspace.M{i}"))
            .collect::<Vec<_>>();
        let result = execute(
            request(&["Alias.missing"], true),
            "view".into(),
            "qualified Project.Owner as Alias",
            &[],
            &modules,
            Default::default(),
            |batch| {
                calls.set(calls.get() + 1);
                let policy = batch.discovery.as_ref().unwrap();
                assert_eq!(policy.eligible_modules, 131);
                assert_eq!(policy.scopes.len(), 129);
                assert_eq!(
                    policy.scopes[0],
                    InspectionScope::PublicModule("Project.Owner".into())
                );
                assert_eq!(policy.scopes[1], InspectionScope::Current);
                let candidates = vec![HostedDiscoveryCandidate {
                    query: "Owner.visible".into(),
                    origins: vec!["Alias.missing".into()],
                    summary: summary.clone(),
                    local: true,
                    reference: Some(IdentifierRef {
                        module: String::new(),
                        name: "Owner.visible".into(),
                        namespace: IdentifierNamespace::Value,
                    }),
                }];
                // A protected/unavailable scope can have no actual search. Receipt
                // submission and actual attempted scopes remain independent.
                let selected = selection(
                    batch,
                    HostedFallback::Attempted(vec![NameScope::Current]),
                    candidates,
                    200,
                    199,
                );
                Ok(HostedLookupOutput {
                    answers: vec![name(
                        0,
                        SelectedLookupAnswer::Missing {
                            export_names: vec![],
                        },
                    )],
                    discovery: Some(selected),
                })
            },
        );
        assert_eq!(calls.get(), 1, "no second compiler request");
        assert_eq!(result.candidates[0].summary, summary);
        assert_eq!(result.candidates[0].reference.as_ref().unwrap().module, "");
        let LookupDiscovery::Observed(receipt) = result.search_receipt.discovery else {
            panic!("issued discovery")
        };
        assert_eq!(receipt.submitted_modules, LookupCount::Known(128));
        assert_eq!(receipt.attempted_scopes, vec![LookupSearchScope::Current]);
        assert_eq!(receipt.eligible_modules, LookupCount::Known(131));
        assert_eq!(receipt.enumerated_occurrences, LookupCount::Known(200));
        assert_eq!(receipt.discarded_occurrences, LookupCount::Known(199));
        assert_eq!(receipt.distinct_discarded, LookupCount::Unknown);
        assert_eq!(receipt.unenumerated_candidates, LookupCount::Unknown);
    }

    #[test]
    fn seed_only_and_type_only_discovery_consume_worker_candidates_without_fallback() {
        for query in ["value", ":: Int -> Int"] {
            let result = execute(
                request(&[query], true),
                "view".into(),
                "",
                &[],
                &[],
                Default::default(),
                |batch| {
                    let answer = if query.starts_with("::") {
                        HostedLookupAnswer::Type {
                            ordinal: 0,
                            matches: SelectedMatches {
                                observed: 0,
                                entries: vec![],
                                limit: 20,
                            },
                        }
                    } else {
                        name(0, selected(vec![entry("value")]))
                    };
                    let candidate = HostedDiscoveryCandidate {
                        query: "Types.Ref".into(),
                        origins: vec![query.into()],
                        summary: "issued seed summary".into(),
                        local: true,
                        reference: None,
                    };
                    Ok(HostedLookupOutput {
                        answers: vec![answer],
                        discovery: Some(selection(
                            batch,
                            HostedFallback::NotNeeded,
                            vec![candidate],
                            1,
                            0,
                        )),
                    })
                },
            );
            assert_eq!(result.candidates.len(), 1);
            assert_eq!(result.candidates[0].summary, "issued seed summary");
            let LookupDiscovery::Observed(receipt) = result.search_receipt.discovery else {
                panic!()
            };
            assert!(matches!(
                receipt.module_search,
                LookupModuleSearch::NotNeeded
            ));
            assert_eq!(receipt.submitted_modules, LookupCount::Known(0));
            assert!(receipt.attempted_scopes.is_empty());
        }
    }

    #[test]
    fn per_query_rejection_keeps_native_candidates_without_isolation() {
        let calls = Cell::new(0);
        let result = execute(
            request(&["bad", ":: Int -> Int"], true),
            "view".into(),
            "",
            &[],
            &[],
            Default::default(),
            |batch| {
                calls.set(calls.get() + 1);
                Ok(HostedLookupOutput {
                    answers: vec![
                        HostedLookupAnswer::Rejected {
                            operation: HostedOperationIdentity::Primary(0),
                            diagnostic: "metadata source rejected".into(),
                        },
                        HostedLookupAnswer::Type {
                            ordinal: 1,
                            matches: SelectedMatches {
                                observed: 0,
                                entries: vec![],
                                limit: 20,
                            },
                        },
                    ],
                    discovery: Some(selection(
                        batch,
                        HostedFallback::Unavailable("metadata source rejected".into()),
                        vec![HostedDiscoveryCandidate {
                            query: "Type.Ref".into(),
                            origins: vec![":: Int -> Int".into()],
                            summary: "type seed survives".into(),
                            local: true,
                            reference: None,
                        }],
                        1,
                        0,
                    )),
                })
            },
        );
        assert_eq!(calls.get(), 1);
        assert!(matches!(
            &result.results[0].outcome,
            LookupOutcome::Rejected(_)
        ));
        assert_eq!(result.candidates.len(), 1);
        let LookupDiscovery::Observed(receipt) = result.search_receipt.discovery else {
            panic!()
        };
        assert!(matches!(
            receipt.module_search,
            LookupModuleSearch::Unavailable
        ));
        assert_eq!(receipt.submitted_modules, LookupCount::Known(0));
        assert!(receipt.attempted_scopes.is_empty());
    }

    #[test]
    fn diagnostics_isolation_disables_discovery_and_keeps_original_positions() {
        let calls = Cell::new(0);
        let result = execute(
            request(&["doc topics", "good", "bad"], true),
            "view".into(),
            "",
            &[],
            &[],
            Default::default(),
            |batch| {
                calls.set(calls.get() + 1);
                if batch.operations.len() > 1 {
                    assert!(batch.discovery.is_some());
                    return Err(source_error("batch source rejected"));
                }
                assert!(
                    batch.discovery.is_none(),
                    "isolation cannot replay discovery"
                );
                match batch.operations[0] {
                    HostedLookupOperation::Primary { ordinal: 1, .. } => {
                        Ok(output(vec![name(1, selected(vec![entry("good")]))]))
                    }
                    HostedLookupOperation::Primary { ordinal: 2, .. } => {
                        Err(source_error("bad source"))
                    }
                    _ => panic!("original position changed"),
                }
            },
        );
        assert_eq!(calls.get(), 3);
        assert!(matches!(
            result.results[1].outcome,
            LookupOutcome::Found(_, false)
        ));
        assert!(
            matches!(&result.results[2].outcome, LookupOutcome::Rejected(diagnostic) if diagnostic.contains("bad source"))
        );
        assert!(matches!(
            result.search_receipt.discovery,
            LookupDiscovery::NotRun(LookupSearchNotRun::Unavailable)
        ));
    }

    #[test]
    fn infrastructure_or_receipt_failure_never_isolates_or_fabricates_missing() {
        for malformed in [false, true] {
            let calls = Cell::new(0);
            let result = execute(
                request(&["a", "b"], true),
                "view".into(),
                "",
                &[],
                &[],
                Default::default(),
                |_| {
                    calls.set(calls.get() + 1);
                    if malformed {
                        Ok(output(vec![]))
                    } else {
                        Err(LookupInspectionError::Message("worker stopped".into()))
                    }
                },
            );
            assert_eq!(calls.get(), 1);
            assert!(result.issue.is_some());
            assert!(result
                .results
                .iter()
                .all(|result| matches!(result.outcome, LookupOutcome::Rejected(_))));
            assert!(result
                .search_receipt
                .matches
                .iter()
                .all(|receipt| receipt.observed == LookupCount::Unknown));
            assert!(matches!(
                result.search_receipt.discovery,
                LookupDiscovery::NotRun(LookupSearchNotRun::Unavailable)
            ));
        }
    }

    #[test]
    fn malformed_typed_worker_receipts_refuse_at_the_actor_frontdoor() {
        for query in ["value", ":: Int"] {
            for mutation in 0..9 {
                let calls = Cell::new(0);
                let result = execute(
                    request(&[query], true),
                    "view".into(),
                    "",
                    &[],
                    &[],
                    Default::default(),
                    |batch| {
                        calls.set(calls.get() + 1);
                        let mut answer = if query.starts_with("::") {
                            HostedLookupAnswer::Type {
                                ordinal: 0,
                                matches: SelectedMatches {
                                    observed: 0,
                                    entries: vec![],
                                    limit: 20,
                                },
                            }
                        } else {
                            name(0, selected(vec![entry("value")]))
                        };
                        let mut discovery =
                            selection(batch, HostedFallback::NotNeeded, vec![], 0, 0);
                        match mutation {
                            0 => match &mut answer {
                                HostedLookupAnswer::Type { matches, .. } => matches.observed = 1,
                                HostedLookupAnswer::Name {
                                    answer: SelectedLookupAnswer::Info(matches),
                                    ..
                                } => matches.observed = 0,
                                _ => unreachable!(),
                            },
                            1 => match &mut answer {
                                HostedLookupAnswer::Type { matches, .. } => matches.limit = 1,
                                HostedLookupAnswer::Name {
                                    answer: SelectedLookupAnswer::Info(matches),
                                    ..
                                } => matches.limit = 1,
                                _ => unreachable!(),
                            },
                            2 => discovery.discarded_occurrences = 1,
                            3 => discovery.candidates.push(HostedDiscoveryCandidate {
                                query: "not enumerated".into(),
                                origins: vec![query.into()],
                                summary: "invalid count".into(),
                                local: true,
                                reference: None,
                            }),
                            4 => discovery.candidate_limit = 0,
                            5 => discovery.submitted_modules = 1,
                            6 => {
                                discovery.fallback =
                                    HostedFallback::Unavailable("no rejected metadata".into())
                            }
                            7 => {
                                discovery.fallback =
                                    HostedFallback::Attempted(vec![NameScope::Current])
                            }
                            8 => match &mut answer {
                                HostedLookupAnswer::Type { ordinal, .. }
                                | HostedLookupAnswer::Name { ordinal, .. } => *ordinal = 1,
                                _ => unreachable!(),
                            },
                            _ => unreachable!(),
                        }
                        Ok(HostedLookupOutput {
                            answers: vec![answer],
                            discovery: Some(discovery),
                        })
                    },
                );
                assert_eq!(calls.get(), 1, "bad receipt is not source rejection");
                assert!(result.issue.is_some(), "query {query} mutation {mutation}");
                assert!(
                    matches!(result.results[0].outcome, LookupOutcome::Rejected(_)),
                    "query {query} mutation {mutation}"
                );
                assert!(result.candidates.is_empty());
            }
        }
        for mutation in 0..8 {
            let mut req = request(&[], false);
            req.references.push(LookupReference {
                module: String::new(),
                name: "Owner.value".into(),
                namespace: LookupNamespace::Value,
            });
            let nominal = |name: &str| {
                let mut hit = entry(name);
                hit.module = Some("Owner".into());
                hit
            };
            let result = execute(req, "view".into(), "", &[], &[], Default::default(), |_| {
                let answer = match mutation {
                    0 => SelectedLookupAnswer::Module {
                        module: "Other".into(),
                        matches: SelectedMatches {
                            observed: 0,
                            entries: vec![],
                            limit: 20,
                        },
                    },
                    1 => SelectedLookupAnswer::ModuleNotFound("Other".into()),
                    2 => SelectedLookupAnswer::Ambiguous(SelectedMatches {
                        observed: 1,
                        entries: vec![nominal("value")],
                        limit: 20,
                    }),
                    3 => {
                        let mut wrong = nominal("value");
                        wrong.kind = "type".into();
                        selected(vec![wrong])
                    }
                    5 => selected(vec![nominal("other")]),
                    6 => {
                        let mut wrong = entry("other");
                        wrong.module = None;
                        selected(vec![wrong])
                    }
                    7 => selected(vec![entry("value")]),
                    _ => {
                        let mut local = entry("value");
                        local.module = None;
                        selected(vec![local])
                    }
                };
                Ok(output(vec![HostedLookupAnswer::Reference {
                    destinations: vec![0],
                    scopes: if mutation == 4 {
                        vec![NameScope::PublicModule("Other".into())]
                    } else {
                        vec![NameScope::Current]
                    },
                    answer,
                }]))
            });
            assert!(
                matches!(result.results[0].outcome, LookupOutcome::Rejected(_)),
                "reference mutation {mutation}"
            );
        }
    }

    #[test]
    fn requested_candidate_cap_is_clamped_once_and_worker_receipt_is_preserved() {
        for requested in [-1, 0, 1, 127, 128, 129] {
            let mut req = request(&["value"], true);
            req.candidate_limit = requested;
            let result = execute(
                req,
                "view".into(),
                "",
                &[],
                &[],
                Default::default(),
                |batch| {
                    let cap = batch.discovery.as_ref().unwrap().candidate_limit;
                    assert_eq!(cap, requested.clamp(0, 128) as u32);
                    Ok(HostedLookupOutput {
                        answers: vec![name(0, selected(vec![entry("value")]))],
                        discovery: Some(selection(
                            batch,
                            if cap == 0 {
                                HostedFallback::ZeroBudget
                            } else {
                                HostedFallback::NotNeeded
                            },
                            vec![],
                            0,
                            0,
                        )),
                    })
                },
            );
            if requested <= 0 {
                assert!(matches!(
                    result.search_receipt.discovery,
                    LookupDiscovery::NotRun(LookupSearchNotRun::ZeroBudget)
                ));
            } else {
                let LookupDiscovery::Observed(receipt) = result.search_receipt.discovery else {
                    panic!()
                };
                assert_eq!(receipt.candidate_limit, requested.min(128));
            }
        }
    }

    #[test]
    fn invalidation_retains_observed_counts_and_search_history_once() {
        let mut result = execute(
            request(&["value"], false),
            "view".into(),
            "",
            &[],
            &[],
            Default::default(),
            |_| Ok(output(vec![name(0, selected(vec![entry("value")]))])),
        );
        result.invalidate_changed_view();
        result.invalidate_changed_view();
        assert!(result.results.is_empty());
        assert!(result.search_receipt.matches.is_empty());
        assert_eq!(
            result.search_receipt.attempted_scopes,
            vec![LookupSearchScope::Current]
        );
        let LookupSearchDisposition::Invalidated(receipts, discovery) =
            result.search_receipt.disposition
        else {
            panic!()
        };
        assert_eq!(receipts[0].observed, LookupCount::Known(1));
        assert!(matches!(discovery, LookupDiscovery::NotRequested));
    }

    #[test]
    fn examples_follow_issued_nominal_identity_and_refuse_shadows_ambiguity_and_unavailable() {
        let workspace = tempfile::tempdir().unwrap();
        let checks = workspace.path().join(".exomonad/workspace/checks");
        std::fs::create_dir_all(&checks).unwrap();
        std::fs::write(checks.join("example.hs"), "Cmd.start command\n").unwrap();
        std::fs::write(checks.join("usage-examples.json"), r#"{"examples":[{"module":"Tidepool.Command","name":"start","namespace":"value","source":"checks/example.hs","prerequisites":[],"requirements":"Import Cmd"}]}"#).unwrap();
        let usage = crate::UsagePointerTable::discover(workspace.path()).unwrap();
        for exact in [false, true] {
            for (owner, availability, live, ambiguous, expected) in [
                (
                    "Tidepool.Command",
                    InspectionAvailability::Available,
                    false,
                    false,
                    true,
                ),
                (
                    "Other.Command",
                    InspectionAvailability::Available,
                    false,
                    false,
                    false,
                ),
                (
                    "Tidepool.Command",
                    InspectionAvailability::Available,
                    true,
                    false,
                    false,
                ),
                (
                    "Tidepool.Command",
                    InspectionAvailability::Unavailable,
                    false,
                    false,
                    false,
                ),
                (
                    "Tidepool.Command",
                    InspectionAvailability::Available,
                    false,
                    true,
                    false,
                ),
            ] {
                let mut req = request(if exact { &[] } else { &["Cmd.start"] }, false);
                if exact {
                    req.references.push(LookupReference {
                        module: "Reexport.Command".into(),
                        name: "start".into(),
                        namespace: LookupNamespace::Value,
                    });
                }
                let result = execute(
                    req,
                    "view".into(),
                    "qualified Tidepool.Command as Cmd",
                    if live {
                        &["Tidepool.Command".into()]
                    } else {
                        &[]
                    },
                    &[],
                    usage.clone(),
                    |batch| {
                        let mut hit = entry("start");
                        hit.module = Some(owner.into());
                        hit.availability = availability;
                        let answer = if ambiguous {
                            let mut other = hit.clone();
                            other.module = Some("Other.Command".into());
                            SelectedLookupAnswer::Ambiguous(SelectedMatches {
                                entries: vec![hit, other],
                                observed: 2,
                                limit: 20,
                            })
                        } else {
                            selected(vec![hit])
                        };
                        Ok(output(vec![match &batch.operations[0] {
                            HostedLookupOperation::Primary { ordinal, .. } => {
                                name(*ordinal, answer)
                            }
                            HostedLookupOperation::ExactReference { destinations, .. } => {
                                HostedLookupAnswer::Reference {
                                    destinations: destinations.clone(),
                                    scopes: vec![NameScope::PublicModule(
                                        "Reexport.Command".into(),
                                    )],
                                    answer,
                                }
                            }
                        }]))
                    },
                );
                match &result.results[0].outcome {
                    LookupOutcome::Found(entries, false)
                    | LookupOutcome::Ambiguous(entries, false) => {
                        assert_eq!(entries[0].example.is_some(), expected);
                        if expected {
                            assert_eq!(
                                entries[0].example.as_ref().unwrap().source,
                                "Cmd.start command\n"
                            );
                        }
                    }
                    _ => panic!("issued example fixture must resolve"),
                }
            }
        }
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
