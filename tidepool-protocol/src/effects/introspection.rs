//! Structured, read-only inspection of the executing resident Haskell scope.

use crate::hs::HsType;
use crate::schema::{
    Arg, Effect, HandlingClass, Helper, HelperBody, JsonInstance, Polymorphism, RecordField,
    RustBinding, SumVariant, TypeDef, TypeShape, VariantFields, Verb, WireDerives,
};

const NO_WIRE: WireDerives = WireDerives(&[]);

#[must_use]
pub fn introspection() -> Effect {
    Effect {
        name: "Introspection",
        authored_surface: crate::schema::AuthoredSurface::All,
        handler: "IntrospectionDecodeHandler",
        handler_module: "introspection",
        req_enum: "IntrospectionReq",
        decl_fn: "introspection_decl",
        description: &[
            "Read-only, GHC-authoritative structured inspection of the executing resident ",
            "scope or an explicitly named public module.",
        ],
        prompt_card: Some(&[
            "Use qualified `Tidepool.Introspection.info`/`typeOf` for structured Haskell API inspection.",
        ]),
        type_params: &[],
        default_row_args: &[],
        helpers_row_polymorphic: true,
        extra_imports: &[],
        type_defs: type_defs(),
        foreign_types: &[],
        errors: None,
        verbs: vec![
            verb(
                "IntrospectionInfoWith",
                "introspection_info_with",
                "query",
                HsType::Named("IdentifierInfo"),
            ),
            verb(
                "IntrospectionTypeOfWith",
                "introspection_type_of_with",
                "query",
                HsType::Named("TypeInfo"),
            ),
        ],
        helpers: vec![
            Helper {
                name: "info",
                ctor: Some("IntrospectionInfoWith"),
                substrate: false,
                doc: &["Inspect a name and return its structured declaration."],
                body: HelperBody::Pointfree,
            },
            Helper {
                name: "typeOf",
                ctor: Some("IntrospectionTypeOfWith"),
                substrate: false,
                doc: &["Inspect a name's canonical type."],
                body: HelperBody::Pointfree,
            },
        ],
        polymorphism: Polymorphism::None,
        dispatched: false,
        caller_principal: false,
    }
}

fn verb(ctor: &'static str, method: &'static str, arg: &'static str, ok: HsType) -> Verb {
    Verb {
        ctor,
        method,
        args: vec![Arg {
            name: arg,
            ty: HsType::Named("NameQuery"),
            rust: RustBinding::HaskellValue,
        }],
        ret: HsType::either(HsType::Named("QueryError"), ok),
        errors: None,
        handling: HandlingClass::Actor,
        extract: None,
    }
}

fn type_defs() -> Vec<TypeDef> {
    vec![
        sum(
            "NameScope",
            vec![variant("CurrentScope", vec![]), variant("PublicModule", vec![HsType::Text])],
            &["The executing lexical scope, or one explicitly named public module."],
        ),
        closed_sum(
            "NameNamespace",
            &["AnyName", "ValueName", "TypeName", "ConstructorName"],
        ),
        record(
            "NameQuery",
            vec![
                field("queryScope", HsType::Named("NameScope")),
                field("queryNamespace", HsType::Named("NameNamespace")),
                field("queryName", HsType::Text),
            ],
            &["A name lookup; arbitrary expressions are deliberately out of scope."],
        ),
        closed_sum(
            "IdentifierNamespace",
            &["ValueIdentifier", "TypeIdentifier", "ConstructorIdentifier", "FieldIdentifier"],
        ),
        record(
            "IdentifierRef",
            vec![
                field("identifierModule", HsType::Text),
                field("identifierName", HsType::Text),
                field("identifierNamespace", HsType::Named("IdentifierNamespace")),
            ],
            &["A resolved, qualified identifier."],
        ),
        record(
            "ScopeProvenance",
            vec![
                field("provenanceScope", HsType::Named("NameScope")),
                field("provenanceGeneration", HsType::Int),
                field("provenanceFingerprint", HsType::Text),
            ],
            &["The immutable compile view used for one inspection."],
        ),
        record(
            "TypeExpression",
            vec![
                field("typeCanonical", HsType::Text),
                field("typeVariables", HsType::list(HsType::Text)),
                field("typeConstraints", HsType::list(HsType::Text)),
            ],
            &["A canonical pretty type plus separately projectable binders and constraints."],
        ),
        record(
            "TypeInfo",
            vec![
                field("typeIdentifier", HsType::Named("IdentifierRef")),
                field("typeExpression", HsType::Named("TypeExpression")),
                field("typeProvenance", HsType::Named("ScopeProvenance")),
            ],
            &["A resolved identifier's canonical type in one immutable compile view."],
        ),
        record(
            "FieldInfo",
            vec![
                field("fieldName", HsType::Text),
                field("fieldType", HsType::Named("TypeExpression")),
            ],
            &[],
        ),
        record(
            "ConstructorInfo",
            vec![
                field("constructorRef", HsType::Named("IdentifierRef")),
                field("constructorType", HsType::Named("TypeExpression")),
                field(
                    "constructorArguments",
                    HsType::list(HsType::Named("TypeExpression")),
                ),
                field("recordFields", HsType::list(HsType::Named("FieldInfo"))),
            ],
            &["Constructor identity, full signature, positional arguments, and visible fields."],
        ),
        record(
            "ClassMethodInfo",
            vec![
                field("classMethodRef", HsType::Named("IdentifierRef")),
                field("classMethodType", HsType::Named("TypeExpression")),
            ],
            &[],
        ),
        sum(
            "DeclarationInfo",
            vec![
                variant("ValueDeclaration", vec![HsType::Named("TypeExpression")]),
                variant(
                    "DataDeclaration",
                    vec![
                        HsType::list(HsType::Text),
                        HsType::list(HsType::Named("ConstructorInfo")),
                    ],
                ),
                variant(
                    "NewtypeDeclaration",
                    vec![HsType::list(HsType::Text), HsType::Named("ConstructorInfo")],
                ),
                variant(
                    "TypeSynonymDeclaration",
                    vec![
                        HsType::list(HsType::Text),
                        HsType::Named("TypeExpression"),
                    ],
                ),
                variant(
                    "ClassDeclaration",
                    vec![
                        HsType::list(HsType::Text),
                        HsType::list(HsType::Named("TypeExpression")),
                        HsType::list(HsType::Named("ClassMethodInfo")),
                    ],
                ),
                variant(
                    "ConstructorDeclaration",
                    vec![HsType::Named("IdentifierRef"), HsType::Named("ConstructorInfo")],
                ),
                variant(
                    "RecordSelectorDeclaration",
                    vec![
                        HsType::Named("IdentifierRef"),
                        HsType::Named("TypeExpression"),
                    ],
                ),
            ],
            &["The declaration shape for the resolved identifier; constructors and selectors retain their parent."],
        ),
        record(
            "IdentifierInfo",
            vec![
                field("inspectedIdentifier", HsType::Named("IdentifierRef")),
                field("identifierDeclaration", HsType::Named("DeclarationInfo")),
                field(
                    "identifierParent",
                    HsType::maybe(HsType::Named("IdentifierRef")),
                ),
                field(
                    "identifierProvenance",
                    HsType::Named("ScopeProvenance"),
                ),
            ],
            &["One resolved identifier and its structured declaration."],
        ),
        sum(
            "QueryError",
            vec![
                variant("UnknownIdentifier", vec![HsType::Named("NameQuery")]),
                variant(
                    "AmbiguousIdentifier",
                    vec![
                        HsType::Named("NameQuery"),
                        HsType::list(HsType::Named("IdentifierRef")),
                    ],
                ),
                variant("UnknownModule", vec![HsType::Text]),
                variant("UnsupportedDeclaration", vec![HsType::Text]),
                variant("CompilerUnavailable", vec![HsType::Text]),
                variant(
                    "ScopeChanged",
                    vec![
                        HsType::Named("ScopeProvenance"),
                        HsType::Named("ScopeProvenance"),
                    ],
                ),
            ],
            &["Typed lookup and infrastructure failures; failed queries never fabricate empty success."],
        ),
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
    fn structured_introspection_schema_is_actor_serviced_and_typed() {
        let effect = introspection();
        assert!(!effect.dispatched);
        assert!(effect.validate().is_ok());
        assert_eq!(
            effect.constructor_signatures(),
            vec![
                "IntrospectionInfoWith :: NameQuery -> Introspection (Either QueryError IdentifierInfo)",
                "IntrospectionTypeOfWith :: NameQuery -> Introspection (Either QueryError TypeInfo)",
            ]
        );
        assert!(effect.verbs.iter().all(|verb| {
            verb.handling == HandlingClass::Actor
                && verb.args.len() == 1
                && verb.args[0].ty == HsType::Named("NameQuery")
        }));

        let declarations = effect.type_def_texts();
        for required in [
            "data NameQuery = NameQuery { queryScope :: NameScope, queryNamespace :: NameNamespace, queryName :: Text } deriving (Show, Eq)",
            "data TypeInfo = TypeInfo { typeIdentifier :: IdentifierRef, typeExpression :: TypeExpression, typeProvenance :: ScopeProvenance } deriving (Show, Eq)",
            "data ConstructorInfo = ConstructorInfo { constructorRef :: IdentifierRef, constructorType :: TypeExpression, constructorArguments :: [TypeExpression], recordFields :: [FieldInfo] } deriving (Show, Eq)",
            "data FieldInfo = FieldInfo { fieldName :: Text, fieldType :: TypeExpression } deriving (Show, Eq)",
        ] {
            assert!(declarations.iter().any(|declaration| declaration == required));
        }
        let declaration_info = declarations
            .iter()
            .find(|declaration| declaration.starts_with("data DeclarationInfo ="))
            .expect("DeclarationInfo must be generated");
        for constructor in [
            "ValueDeclaration",
            "DataDeclaration",
            "NewtypeDeclaration",
            "TypeSynonymDeclaration",
            "ClassDeclaration",
            "ConstructorDeclaration",
            "RecordSelectorDeclaration",
        ] {
            assert!(declaration_info.contains(constructor));
        }
        let query_error = declarations
            .iter()
            .find(|declaration| declaration.starts_with("data QueryError ="))
            .expect("QueryError must be generated");
        for outcome in [
            "UnknownIdentifier",
            "AmbiguousIdentifier",
            "UnknownModule",
            "UnsupportedDeclaration",
            "CompilerUnavailable",
            "ScopeChanged",
        ] {
            assert!(query_error.contains(outcome));
        }
    }
}
