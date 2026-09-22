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
            "Read-only, GHC-authoritative structured inspection of the executing resident ",
            "scope or an explicitly named public module.",
        ],
        prompt_card: Some(&[
            "Use `Tidepool.Lookup.lookupRaw` for structured lookup without automatic enrichment.",
        ]),
        type_params: &[],
        default_row_args: &[],
        helpers_row_polymorphic: true,
        extra_imports: &[],
        type_defs: type_defs(),
        foreign_types: &[],
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
            doc: &["Execute structured lookups without presentation or enrichment."],
            body: HelperBody::Pointfree,
        }],
        polymorphism: Polymorphism::None,
        generated_handler: false,
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
        extract: None,
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
            ],
            &[],
        ),
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
