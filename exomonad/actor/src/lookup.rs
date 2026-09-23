//! Structured lookup mechanics shared by the self-hosted lookup tool.
use crate::lookup_tool;
use tidepool_bridge_derive::{FromHaskell, ToHaskell};
use tidepool_runtime::session::{InspectionQuery, InspectionResult};

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
                    attempted
                        .into_iter()
                        .map(|a| {
                            match a {
                                lookup_tool::LookupInterpretation::Name => "name",
                                lookup_tool::LookupInterpretation::Module => "module",
                                lookup_tool::LookupInterpretation::TypeSearch => "type search",
                            }
                            .into()
                        })
                        .collect(),
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

pub(crate) fn execute(
    request: LookupRequest,
    view: String,
    imports: &str,
    live_modules: &[String],
    workspace_modules: &[String],
    usage: crate::UsagePointerTable,
    inspect: impl Fn(&[InspectionQuery]) -> Result<Vec<InspectionResult>, String>,
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
    let requests = queries(&prepared, imports);
    let mut inspected = match inspect(&requests) {
        Ok(results) if results.len() == requests.len() => results,
        Ok(_) => {
            batch.issue = Some("lookup compiler returned an incomplete batch".into());
            return batch;
        }
        Err(error) => {
            batch.issue = Some(error);
            return batch;
        }
    };
    // Public exports are valid read-only lookup targets even without a source import.
    for index in 0..requests.len().saturating_sub(1) {
        if let (
            InspectionQuery::Info(name),
            InspectionResult::NotFound { .. },
            InspectionResult::Browse { entries, .. },
        ) = (&requests[index], &inspected[index], &inspected[index + 1])
        {
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
    let mut offset = 0;
    let mut fallbacks = vec![];
    for query in &prepared {
        if let lookup_tool::PreparedLookupKind::Qualified(name) = &query.kind {
            if matches!(inspected[offset], InspectionResult::NotFound { .. }) {
                if let Some((qualifier, leaf)) = name.rsplit_once('.') {
                    let module = lookup_tool::resolve_qualifier_module(imports, qualifier)
                        .unwrap_or_else(|| qualifier.into());
                    fallbacks.push((
                        offset,
                        name.clone(),
                        leaf.to_owned(),
                        InspectionQuery::Browse {
                            module,
                            expanded: false,
                        },
                    ));
                }
            }
        }
        offset += queries(std::slice::from_ref(query), imports).len();
    }
    if !fallbacks.is_empty() {
        if let Ok(results) = inspect(
            &fallbacks
                .iter()
                .map(|(_, _, _, query)| query.clone())
                .collect::<Vec<_>>(),
        ) {
            for ((offset, name, leaf, _), result) in fallbacks.into_iter().zip(results) {
                if let InspectionResult::Browse { entries, .. } = result {
                    let exact = entries
                        .into_iter()
                        .filter(|entry| entry.name == leaf)
                        .collect::<Vec<_>>();
                    if !exact.is_empty() {
                        inspected[offset] = if exact.len() > 1 {
                            InspectionResult::Ambiguous {
                                query: name,
                                entries: exact,
                            }
                        } else {
                            InspectionResult::Info {
                                query: name,
                                entries: exact,
                            }
                        };
                    }
                }
            }
        }
    }
    let mut response = lookup_tool::resolve(
        prepared.clone(),
        inspected.clone(),
        live_modules,
        workspace_modules,
        usage.clone(),
    );
    let reference_results = if request.references.is_empty() {
        Ok(vec![])
    } else {
        inspect(
            &request
                .references
                .iter()
                .map(|reference| {
                    if reference.module.is_empty() {
                        InspectionQuery::ScopeBrowse
                    } else {
                        InspectionQuery::Browse {
                            module: reference.module.clone(),
                            expanded: true,
                        }
                    }
                })
                .collect::<Vec<_>>(),
        )
    };
    for (index, reference) in request.references.iter().enumerate() {
        let query = if reference.module.is_empty() {
            reference.name.clone()
        } else {
            format!("{}.{}", reference.module, reference.name)
        };
        let found = reference_results
            .as_ref()
            .map(|results| results.get(index).cloned())
            .map_err(Clone::clone);
        let outcome = match found {
            Ok(result) => match result {
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
                Some(other) => lookup_tool::LookupOutcome::Rejected {
                    diagnostic: other.render(),
                },
                None => lookup_tool::LookupOutcome::Rejected {
                    diagnostic: "lookup compiler omitted reference result".into(),
                },
            },
            Err(diagnostic) => lookup_tool::LookupOutcome::Rejected { diagnostic },
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
        let exports = if misses {
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
            match inspect(&discovery_queries) {
                Ok(results) => results,
                Err(error) => {
                    batch.issue = Some(format!("lookup candidate discovery unavailable: {error}"));
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
    batch
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use tidepool_runtime::session::{
        IdentifierNamespace, IdentifierRef, InfoEntry, InspectionAvailability,
    };
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
