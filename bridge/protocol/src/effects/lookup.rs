//! Structured, read-only inspection of the executing resident Haskell scope.

use crate::hs::HsType;
use crate::schema::{
    Arg, Effect, HandlingClass, Helper, HelperBody, JsonInstance, Polymorphism, RecordField,
    RustBinding, SumVariant, TypeDef, TypeShape, VariantFields, Verb, WireDerives,
};

const NO_WIRE: WireDerives = WireDerives(&[]);

#[must_use]
pub fn lookup() -> Effect {
    Effect {
        name: "Lookup",
        authored_surface: crate::schema::AuthoredSurface::All,
        handler: "LookupDecodeHandler",
        handler_module: "lookup",
        req_enum: "LookupReq",
        decl_fn: "lookup_decl",
        description: &[
            "Type-directed discovery of the executing resident scope or an explicitly named public module, checked by GHC. ",
            "Use names when known, `::` queries with `_` holes when the needed type is known, and module exports or documentation for unfamiliar abstractions. ",
            "`Tidepool.Lookup.lookupRaw` returns captured structured results without automatic related queries; preserve each query's missing, ambiguous, rejected or found result, and inspect found entries' availability separately. ",
            "Lookup inspects an immutable scope view; a declaration's availability under an effect row is separate from runtime resource authority. ",
            "Use the hosted lookup tool for direct questions, and the structured API to filter candidates or build repeated discovery programs.",
        ],
        prompt_card: Some(&[
            "`Tidepool.Lookup.lookupRaw` returns structured, GHC-checked lookup from the current scope. Search names, `::` types with `_` holes, or docs; keep ambiguity and availability explicit. ",
            "Use hosted lookup for direct discovery, structured results for programmatic filtering. Finding a signature does not grant its effects or resource authority.",
        ]),
        type_params: &[],
        default_row_args: &[],
        extra_imports: &[],
        type_defs: type_defs(),
        external_types: &[],
        errors: None,
        verbs: vec![verb(
            "LookupRaw",
            "lookup_raw",
            "request",
            HsType::Named("LookupBatch"),
        )],
        helpers: vec![Helper {
            name: "lookupRaw",
            ctor: Some("LookupRaw"),
            substrate: false,
            doc: &["Capture structured lookups without presentation or automatic related queries."],
            body: HelperBody::Pointfree,
        }],
        polymorphism: Polymorphism::None,
        generated_handler: false,
        handler_execution: crate::schema::HandlerExecution::Immediate,
        caller_principal: false,
    }
}

fn verb(ctor: &'static str, method: &'static str, arg: &'static str, ok: HsType) -> Verb {
    Verb {
        ctor,
        method,
        args: vec![Arg {
            name: arg,
            ty: HsType::Named("LookupRequest"),
            rust: RustBinding::HaskellValue,
        }],
        ret: ok,
        errors: None,
        handling: HandlingClass::Actor,
    }
}

fn type_defs() -> Vec<TypeDef> {
    vec![
        record(
            "LookupRequest",
            vec![
                field("lookupQueries", HsType::list(HsType::Text)),
                field("lookupDiscover", HsType::Bool),
                field("lookupExpectedView", HsType::maybe(HsType::Text)),
                field("lookupCandidateLimit", HsType::Int),
                field(
                    "lookupReferences",
                    HsType::list(HsType::Named("LookupReference")),
                ),
            ],
            &[],
        ),
        record(
            "LookupBatch",
            vec![
                field("lookupResults", HsType::list(HsType::Named("LookupResult"))),
                field(
                    "lookupCandidates",
                    HsType::list(HsType::Named("LookupCandidate")),
                ),
                field("lookupView", HsType::Text),
                field("lookupIssue", HsType::maybe(HsType::Text)),
                field("lookupIdentity", HsType::Text),
                field("lookupSearchReceipt", HsType::Named("LookupSearchReceipt")),
            ],
            &[],
        ),
        record("LookupSearchReceipt", vec![
            field("searchView", HsType::Text),
            field("searchDisposition", HsType::Named("LookupSearchDisposition")),
            field("searchAttemptedScopes", HsType::list(HsType::Named("LookupSearchScope"))),
            field("searchMatches", HsType::list(HsType::Named("LookupMatchReceipt"))),
            field("searchDiscovery", HsType::Named("LookupDiscovery")),
        ], &["Owner-issued observations for this captured view, distinct from display omission. Attempted scopes are not a claim of successful or universal coverage."]),
        sum("LookupSearchDisposition", vec![
            variant("LookupSearchNotExecuted", vec![]),
            variant("LookupSearchCaptured", vec![]),
            variant("LookupSearchInvalidated", vec![HsType::list(HsType::Named("LookupMatchReceipt")), HsType::Named("LookupDiscovery")]),
        ], &["Invalidated observations describe executed work only. Their hits were refused and are not retained for selection; accepted result and match lists are empty."]),
        sum("LookupSearchScope", vec![
            variant("LookupCurrentScope", vec![]),
            variant("LookupPublicModule", vec![HsType::Text]),
        ], &[]),
        sum("LookupCount", vec![
            variant("LookupKnownCount", vec![HsType::Int]),
            variant("LookupUnknownCount", vec![]),
        ], &["Unknown is not zero. Counts describe only the issuing observation, never an unsearched universe."]),
        record("LookupMatchReceipt", vec![
            field("matchQuery", HsType::Text),
            field("matchObservedOccurrences", HsType::Named("LookupCount")),
            field("matchRetainedOccurrences", HsType::Named("LookupCount")),
            field("matchLimit", HsType::maybe(HsType::Int)),
        ], &["Resolved entry occurrences before and after the native match cap. Discarded matches are not retained for later selection."]),
        sum("LookupDiscovery", vec![
            variant("LookupDiscoveryNotRequested", vec![]),
            variant("LookupDiscoveryNotRun", vec![HsType::Named("LookupSearchNotRun")]),
            variant("LookupDiscoveryObserved", vec![HsType::Named("LookupDiscoveryReceipt")]),
            variant("LookupDiscoveryInvalidated", vec![]),
        ], &[]),
        closed_sum("LookupSearchNotRun", &[
            "LookupSearchStaleView", "LookupSearchInvalidRequest", "LookupSearchUnavailable",
            "LookupSearchZeroBudget", "LookupSearchNoEligibleQuery",
        ]),
        closed_sum("LookupModuleSearch", &[
            "LookupModulesNotNeeded", "LookupModulesUnavailable", "LookupModulesAttempted",
        ]),
        record("LookupDiscoveryReceipt", vec![
            field("discoveryModuleSearch", HsType::Named("LookupModuleSearch")),
            field("discoveryAttemptedScopes", HsType::list(HsType::Named("LookupSearchScope"))),
            field("discoveryEligibleModules", HsType::Named("LookupCount")),
            field("discoverySubmittedModules", HsType::Named("LookupCount")),
            field("discoveryModuleLimit", HsType::Int),
            field("discoveryEnumeratedOccurrences", HsType::Named("LookupCount")),
            field("discoveryRetainedCandidates", HsType::Named("LookupCount")),
            field("discoveryDiscardedOccurrences", HsType::Named("LookupCount")),
            field("discoveryDistinctDiscarded", HsType::Named("LookupCount")),
            field("discoveryUnenumeratedCandidates", HsType::Named("LookupCount")),
            field("discoveryCandidateLimit", HsType::Int),
            field("discoveryBroaderModuleCount", HsType::Named("LookupCount")),
            field("discoveryIssue", HsType::maybe(HsType::Text)),
        ], &["Eligible modules are the distinct named/workspace inventory before the module cap, not all packages. Candidate occurrences are queued from retained matches or observed exports, after excluding original hits; duplicate origins coalesce. References of discarded matches are not enumerated. Discarded occurrences are not a recoverable frontier, and distinct discarded identities remain unknown unless none were discarded."]),
        record(
            "LookupResult",
            vec![
                field("lookupQuery", HsType::Text),
                field("lookupOutcome", HsType::Named("LookupOutcome")),
            ],
            &[],
        ),
        sum(
            "LookupOutcome",
            vec![
                variant(
                    "LookupFound",
                    vec![HsType::list(HsType::Named("LookupEntry")), HsType::Bool],
                ),
                variant(
                    "LookupMissing",
                    vec![HsType::list(HsType::Text), HsType::list(HsType::Text)],
                ),
                variant(
                    "LookupAmbiguous",
                    vec![HsType::list(HsType::Named("LookupEntry")), HsType::Bool],
                ),
                variant("LookupRejected", vec![HsType::Text]),
            ],
            &[],
        ),
        record(
            "LookupEntry",
            vec![
                field("lookupName", HsType::Text),
                field("lookupModule", HsType::maybe(HsType::Text)),
                field("lookupDeclaration", HsType::Text),
                field("lookupKind", HsType::Named("LookupKind")),
                field("lookupAvailability", HsType::Named("LookupAvailability")),
                field("lookupOrigin", HsType::Named("LookupOrigin")),
                field("lookupQuality", HsType::Named("LookupQuality")),
                field("lookupUsage", HsType::maybe(HsType::Text)),
                field("lookupContract", HsType::maybe(HsType::Text)),
                field(
                    "lookupExample",
                    HsType::maybe(HsType::Named("LookupExample")),
                ),
            ],
            &[],
        ),
        record(
            "LookupExample",
            vec![
                field("exampleLocator", HsType::Text),
                field("exampleRequirements", HsType::Text),
                field("examplePrerequisites", HsType::list(HsType::Text)),
                field("exampleSource", HsType::Text),
                field("exampleSourceIdentity", HsType::maybe(HsType::Text)),
                field("exampleSourceIssue", HsType::maybe(HsType::Text)),
                field("exampleSetup", HsType::list(HsType::Named("LookupSource"))),
            ],
            &[],
        ),
        record(
            "LookupSource",
            vec![
                field("lookupSourceLocator", HsType::Text),
                field("lookupSourceIdentity", HsType::maybe(HsType::Text)),
                field("lookupSourceText", HsType::maybe(HsType::Text)),
                field("lookupSourceIssue", HsType::maybe(HsType::Text)),
            ],
            &[],
        ),
        record(
            "LookupCandidate",
            vec![
                field("candidateQuery", HsType::Text),
                field("candidateOrigins", HsType::list(HsType::Text)),
                field("candidateSummary", HsType::Text),
                field("candidateLocal", HsType::Bool),
                field(
                    "candidateReference",
                    HsType::maybe(HsType::Named("LookupReference")),
                ),
            ],
            &[],
        ),
        record(
            "LookupReference",
            vec![
                field("referenceModule", HsType::Text),
                field("referenceName", HsType::Text),
                field("referenceNamespace", HsType::Named("LookupNamespace")),
            ],
            &[],
        ),
        closed_sum(
            "LookupNamespace",
            &[
                "LookupValueNamespace",
                "LookupTypeNamespace",
                "LookupConstructorNamespace",
                "LookupFieldNamespace",
            ],
        ),
        closed_sum(
            "LookupKind",
            &[
                "LookupValue",
                "LookupClassMethod",
                "LookupRecordSelector",
                "LookupConstructor",
                "LookupType",
                "LookupCoercion",
                "LookupDocumentation",
            ],
        ),
        closed_sum(
            "LookupAvailability",
            &[
                "LookupAvailable",
                "LookupPolymorphic",
                "LookupUnknown",
                "LookupUnavailable",
            ],
        ),
        closed_sum(
            "LookupOrigin",
            &[
                "LookupModuleExport",
                "LookupLiveBinding",
                "LookupDocumentationOrigin",
            ],
        ),
        closed_sum("LookupQuality", &["LookupExact", "LookupUsable"]),
    ]
}
fn field(hs_name: &'static str, ty: HsType) -> RecordField {
    RecordField {
        hs_name,
        rust_name: hs_name,
        ty,
        doc: &[],
    }
}

fn record(name: &'static str, fields: Vec<RecordField>, doc: &'static [&'static str]) -> TypeDef {
    TypeDef {
        name,
        wire_rust: None,
        haskell_module: None,
        shape: TypeShape::Record { fields },
        json: JsonInstance::None,
        derives: NO_WIRE,
        domain: None,
        doc,
    }
}

fn variant(ctor: &'static str, fields: Vec<HsType>) -> SumVariant {
    SumVariant {
        ctor,
        fields: VariantFields::Positional(fields),
        doc: &[],
    }
}

fn sum(name: &'static str, variants: Vec<SumVariant>, doc: &'static [&'static str]) -> TypeDef {
    TypeDef {
        name,
        wire_rust: None,
        haskell_module: None,
        shape: TypeShape::Sum { variants },
        json: JsonInstance::None,
        derives: NO_WIRE,
        domain: None,
        doc,
    }
}

fn closed_sum(name: &'static str, constructors: &'static [&'static str]) -> TypeDef {
    sum(
        name,
        constructors
            .iter()
            .map(|ctor| variant(ctor, vec![]))
            .collect(),
        &[],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lookup_is_actor_serviced_and_retains_typed_reference_identity() {
        let effect = lookup();
        assert!(effect.validate().is_ok());
        assert!(!effect.generated_handler);
        assert_eq!(
            effect.constructor_signatures(),
            vec!["LookupRaw :: LookupRequest -> Lookup LookupBatch"]
        );
        let declarations = effect.type_def_texts();
        assert!(declarations
            .iter()
            .any(|d| d.contains("candidateReference :: Maybe LookupReference")));
        assert!(declarations
            .iter()
            .any(|d| d.contains("lookupReferences :: [LookupReference]")));
        assert!(effect
            .verbs
            .iter()
            .all(|v| v.handling == HandlingClass::Actor));
    }
}
