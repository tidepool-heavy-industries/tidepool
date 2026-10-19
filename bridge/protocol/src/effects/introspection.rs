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
        extra_imports: &[],
        type_defs: type_defs(),
        external_types: &[],
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
            verb_with(
                "CurrentScopeView",
                "introspection_current_scope_view",
                vec![],
                HsType::Named("ScopeProvenance"),
            ),
            verb_with(
                "ScopeBindings",
                "introspection_scope_bindings",
                vec![expected_scope_arg()],
                HsType::Named("NameList"),
            ),
            verb_with(
                "ModuleExports",
                "introspection_module_exports",
                vec![
                    expected_scope_arg(),
                    Arg {
                        name: "module",
                        ty: HsType::Text,
                        rust: RustBinding::Derived,
                    },
                ],
                HsType::Named("NameList"),
            ),
            verb_with(
                "RowFit",
                "introspection_row_fit",
                vec![
                    expected_scope_arg(),
                    Arg {
                        name: "identifier",
                        ty: HsType::Named("IdentifierRef"),
                        rust: RustBinding::HaskellValue,
                    },
                ],
                HsType::Named("Fit"),
            ),
            verb_with(
                "TypeOfExpression",
                "introspection_type_of_expression",
                vec![
                    expected_scope_arg(),
                    Arg {
                        name: "expression",
                        ty: HsType::Text,
                        rust: RustBinding::Derived,
                    },
                ],
                HsType::Named("ExpressionType"),
            ),
            verb_with(
                "TypeSearch",
                "introspection_type_search",
                vec![
                    expected_scope_arg(),
                    Arg {
                        name: "typePattern",
                        ty: HsType::Text,
                        rust: RustBinding::Derived,
                    },
                ],
                HsType::Named("TypeMatches"),
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
            Helper {
                name: "currentScopeView",
                ctor: Some("CurrentScopeView"),
                substrate: false,
                doc: &["Read the immutable compile view this actor's inspection answers are relative to."],
                body: HelperBody::Nullary,
            },
            Helper {
                name: "scopeBindings",
                ctor: Some("ScopeBindings"),
                substrate: false,
                doc: &["List the identifiers bound in the executing scope without checking a target. `Nothing` observes the actor's current compile view; `Just p` refuses with `ScopeChanged` if that compile view is no longer current."],
                body: HelperBody::Applied(&["expectedScope"]),
            },
            Helper {
                name: "moduleExports",
                ctor: Some("ModuleExports"),
                substrate: false,
                doc: &["List a public module's exports without checking a target. Generated session modules remain private. `Nothing` observes the actor's current compile view; `Just p` refuses with `ScopeChanged` if that compile view is no longer current."],
                body: HelperBody::Applied(&["expectedScope", "module"]),
            },
            Helper {
                name: "rowFit",
                ctor: Some("RowFit"),
                substrate: false,
                doc: &["Observe whether an identifier's effects fit the executing actor's row. Runtime resource grants remain separate. `Nothing` observes the actor's current compile view; `Just p` refuses with `ScopeChanged` if that compile view is no longer current."],
                body: HelperBody::Applied(&["expectedScope", "identifier"]),
            },
            Helper {
                name: "typeOfExpression",
                ctor: Some("TypeOfExpression"),
                substrate: false,
                doc: &["Compile an expression probe and return its canonical type without evaluating it. `Nothing` observes the actor's current compile view; `Just p` refuses with `ScopeChanged` if that compile view is no longer current."],
                body: HelperBody::Applied(&["expectedScope", "expression"]),
            },
            Helper {
                name: "typeSearch",
                ctor: Some("TypeSearch"),
                substrate: false,
                doc: &["Compile a type-pattern probe and return matching values without evaluating them. `Nothing` observes the actor's current compile view; `Just p` refuses with `ScopeChanged` if that compile view is no longer current."],
                body: HelperBody::Applied(&["expectedScope", "typePattern"]),
            },
        ],
        polymorphism: Polymorphism::None,
        generated_handler: false,
        handler_execution: crate::schema::HandlerExecution::Immediate,
        caller_principal: false,
    }
}

fn verb(ctor: &'static str, method: &'static str, arg: &'static str, ok: HsType) -> Verb {
    verb_with(
        ctor,
        method,
        vec![Arg {
            name: arg,
            ty: HsType::Named("NameQuery"),
            rust: RustBinding::HaskellValue,
        }],
        ok,
    )
}

fn verb_with(ctor: &'static str, method: &'static str, args: Vec<Arg>, ok: HsType) -> Verb {
    Verb {
        ctor,
        method,
        args,
        ret: HsType::either(HsType::Named("QueryError"), ok),
        errors: None,
        handling: HandlingClass::Actor,
    }
}

fn expected_scope_arg() -> Arg {
    Arg {
        name: "expectedScope",
        ty: HsType::maybe(HsType::Named("ScopeProvenance")),
        rust: RustBinding::HaskellValue,
    }
}

fn type_defs() -> Vec<TypeDef> {
    vec![
        sum(
            "NameScope",
            vec![variant("CurrentScope", vec![]), variant("PublicModule", vec![HsType::Text])],
            &["The executing lexical scope, or one explicitly named public module. Generated session modules are private."],
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
        record(
            "NameEntry",
            vec![
                field("nameEntryName", HsType::Text),
                field("nameEntryModule", HsType::maybe(HsType::Text)),
                field("nameEntryKind", HsType::Text),
                field("nameEntryDisplay", HsType::Text),
                field("nameEntryAvailability", HsType::Named("FitAvailability")),
                field(
                    "nameEntryReferences",
                    HsType::list(HsType::Named("IdentifierRef")),
                ),
                field("nameEntryReference", HsType::Named("IdentifierRef")),
            ],
            &["One enumerated name with its declaration identity, presentation, row availability, and the references it names or resolves."],
        ),
        record(
            "NameList",
            vec![
                field("entries", HsType::list(HsType::Named("NameEntry"))),
                field("namesObserved", HsType::Int),
                field("namesLimit", HsType::Int),
                field("nameListProvenance", HsType::Named("ScopeProvenance")),
            ],
            &["Name entries retained from one enumeration, with the complete count observed before admission, the applied limit, and the compile view the answer describes."],
        ),
        closed_sum(
            "FitAvailability",
            &[
                "FitAvailable",
                "FitPolymorphic",
                "FitUnavailable",
                "FitUnknown",
            ],
        ),
        record(
            "Fit",
            vec![
                field("fitAvailability", HsType::Named("FitAvailability")),
                field("fitIdentifier", HsType::Named("IdentifierRef")),
                field("fitProvenance", HsType::Named("ScopeProvenance")),
            ],
            &["One row-fit observation: the availability outcome together with the identifier it describes and the compile view it was measured in."],
        ),
        record(
            "ExpressionType",
            vec![
                field("expressionSource", HsType::Text),
                field("expressionDisplay", HsType::Text),
                field("expressionAvailability", HsType::Named("FitAvailability")),
                field("expressionProvenance", HsType::Named("ScopeProvenance")),
            ],
            &["One compiled probe expression with its rendered type, row availability, and compile-view provenance. The probe is never evaluated."],
        ),
        closed_sum("TypeMatchQuality", &["TypeMatchExact", "TypeMatchUsable"]),
        record(
            "TypeMatch",
            vec![
                field("typeMatchName", HsType::Text),
                field("typeMatchModule", HsType::maybe(HsType::Text)),
                field("typeMatchSignature", HsType::Text),
                field("typeMatchQuality", HsType::Named("TypeMatchQuality")),
                field("typeMatchAvailability", HsType::Named("FitAvailability")),
                field(
                    "typeMatchReferences",
                    HsType::list(HsType::Named("IdentifierRef")),
                ),
                field("typeMatchReference", HsType::Named("IdentifierRef")),
            ],
            &["One value whose type matches a probe pattern, with its rendered signature, match quality, row availability, and the references it names or resolves."],
        ),
        record(
            "TypeMatches",
            vec![
                field("typeMatches", HsType::list(HsType::Named("TypeMatch"))),
                field("typeMatchesObserved", HsType::Int),
                field("typeMatchesLimit", HsType::Int),
                field("typeMatchesProvenance", HsType::Named("ScopeProvenance")),
            ],
            &["Type matches retained from one probe, with the complete count observed before admission, the applied limit, and the compile view the answer describes."],
        ),
        record(
            "DiagnosticSpan",
            vec![
                field("spanFile", HsType::Text),
                field("spanStartLine", HsType::Int),
                field("spanStartCol", HsType::Int),
                field("spanEndLine", HsType::Int),
                field("spanEndCol", HsType::Int),
            ],
            &["A concrete source span attached to a compiler diagnostic."],
        ),
        closed_sum(
            "DiagnosticSeverity",
            &["DiagnosticError", "DiagnosticWarning"],
        ),
        record(
            "Diagnostic",
            vec![
                field("diagnosticSpan", HsType::maybe(HsType::Named("DiagnosticSpan"))),
                field(
                    "diagnosticSeverity",
                    HsType::Named("DiagnosticSeverity"),
                ),
                field("diagnosticMessage", HsType::Text),
            ],
            &["One compiler diagnostic. A missing span means GHC supplied no usable source location, not that the message is absent."],
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
                variant(
                    "ExpressionRejected",
                    vec![HsType::list(HsType::Named("Diagnostic"))],
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
        assert!(!effect.generated_handler);
        assert!(effect.validate().is_ok());
        assert_eq!(
            effect.constructor_signatures(),
            vec![
                "IntrospectionInfoWith :: NameQuery -> Introspection (Either QueryError IdentifierInfo)",
                "IntrospectionTypeOfWith :: NameQuery -> Introspection (Either QueryError TypeInfo)",
                "CurrentScopeView :: Introspection (Either QueryError ScopeProvenance)",
                "ScopeBindings :: Maybe ScopeProvenance -> Introspection (Either QueryError NameList)",
                "ModuleExports :: Maybe ScopeProvenance -> Text -> Introspection (Either QueryError NameList)",
                "RowFit :: Maybe ScopeProvenance -> IdentifierRef -> Introspection (Either QueryError Fit)",
                "TypeOfExpression :: Maybe ScopeProvenance -> Text -> Introspection (Either QueryError ExpressionType)",
                "TypeSearch :: Maybe ScopeProvenance -> Text -> Introspection (Either QueryError TypeMatches)",
            ]
        );
        assert!(effect
            .verbs
            .iter()
            .all(|verb| verb.handling == HandlingClass::Actor));
        assert!(effect.verbs.iter().all(|verb| matches!(
            &verb.ret,
            HsType::Either(error, _) if **error == HsType::Named("QueryError")
        )));
        assert_eq!(
            effect
                .helpers
                .iter()
                .map(|helper| helper.signature(&effect))
                .collect::<Vec<_>>(),
            vec![
                "info :: forall effs. Member Introspection effs => NameQuery -> Eff effs (Either QueryError IdentifierInfo)",
                "typeOf :: forall effs. Member Introspection effs => NameQuery -> Eff effs (Either QueryError TypeInfo)",
                "currentScopeView :: forall effs. Member Introspection effs => Eff effs (Either QueryError ScopeProvenance)",
                "scopeBindings :: forall effs. Member Introspection effs => Maybe ScopeProvenance -> Eff effs (Either QueryError NameList)",
                "moduleExports :: forall effs. Member Introspection effs => Maybe ScopeProvenance -> Text -> Eff effs (Either QueryError NameList)",
                "rowFit :: forall effs. Member Introspection effs => Maybe ScopeProvenance -> IdentifierRef -> Eff effs (Either QueryError Fit)",
                "typeOfExpression :: forall effs. Member Introspection effs => Maybe ScopeProvenance -> Text -> Eff effs (Either QueryError ExpressionType)",
                "typeSearch :: forall effs. Member Introspection effs => Maybe ScopeProvenance -> Text -> Eff effs (Either QueryError TypeMatches)",
            ]
        );

        let declarations = effect.type_def_texts();
        for required in [
            "data NameQuery = NameQuery { queryScope :: NameScope, queryNamespace :: NameNamespace, queryName :: Text } deriving (Show, Eq)",
            "data TypeInfo = TypeInfo { typeIdentifier :: IdentifierRef, typeExpression :: TypeExpression, typeProvenance :: ScopeProvenance } deriving (Show, Eq)",
            "data ConstructorInfo = ConstructorInfo { constructorRef :: IdentifierRef, constructorType :: TypeExpression, constructorArguments :: [TypeExpression], recordFields :: [FieldInfo] } deriving (Show, Eq)",
            "data FieldInfo = FieldInfo { fieldName :: Text, fieldType :: TypeExpression } deriving (Show, Eq)",
            "data NameEntry = NameEntry { nameEntryName :: Text, nameEntryModule :: Maybe Text, nameEntryKind :: Text, nameEntryDisplay :: Text, nameEntryAvailability :: FitAvailability, nameEntryReferences :: [IdentifierRef], nameEntryReference :: IdentifierRef } deriving (Show, Eq)",
            "data NameList = NameList { entries :: [NameEntry], namesObserved :: Int, namesLimit :: Int, nameListProvenance :: ScopeProvenance } deriving (Show, Eq)",
            "data FitAvailability = FitAvailable | FitPolymorphic | FitUnavailable | FitUnknown deriving (Show, Eq)",
            "data Fit = Fit { fitAvailability :: FitAvailability, fitIdentifier :: IdentifierRef, fitProvenance :: ScopeProvenance } deriving (Show, Eq)",
            "data ExpressionType = ExpressionType { expressionSource :: Text, expressionDisplay :: Text, expressionAvailability :: FitAvailability, expressionProvenance :: ScopeProvenance } deriving (Show, Eq)",
            "data TypeMatchQuality = TypeMatchExact | TypeMatchUsable deriving (Show, Eq)",
            "data TypeMatch = TypeMatch { typeMatchName :: Text, typeMatchModule :: Maybe Text, typeMatchSignature :: Text, typeMatchQuality :: TypeMatchQuality, typeMatchAvailability :: FitAvailability, typeMatchReferences :: [IdentifierRef], typeMatchReference :: IdentifierRef } deriving (Show, Eq)",
            "data TypeMatches = TypeMatches { typeMatches :: [TypeMatch], typeMatchesObserved :: Int, typeMatchesLimit :: Int, typeMatchesProvenance :: ScopeProvenance } deriving (Show, Eq)",
            "data DiagnosticSpan = DiagnosticSpan { spanFile :: Text, spanStartLine :: Int, spanStartCol :: Int, spanEndLine :: Int, spanEndCol :: Int } deriving (Show, Eq)",
            "data DiagnosticSeverity = DiagnosticError | DiagnosticWarning deriving (Show, Eq)",
            "data Diagnostic = Diagnostic { diagnosticSpan :: Maybe DiagnosticSpan, diagnosticSeverity :: DiagnosticSeverity, diagnosticMessage :: Text } deriving (Show, Eq)",
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
            "ExpressionRejected",
        ] {
            assert!(query_error.contains(outcome));
        }
    }
}
