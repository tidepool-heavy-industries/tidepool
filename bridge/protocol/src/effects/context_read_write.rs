//! The typed, synchronous-tool context document boundary.

use crate::hs::HsType;
use crate::schema::{
    Arg, AuthoredSurface, Effect, HandlingClass, Helper, HelperBody, JsonInstance, Polymorphism,
    RecordField, RustBinding, SumVariant, TypeDef, TypeShape, Validation, Verb, WireDerive,
    WireDerives,
};
use crate::types::{IdentityPayload, VariantFields};

const WIRE: WireDerives = WireDerives(&[
    WireDerive::ToHaskell,
    WireDerive::FromHaskell,
    WireDerive::Clone,
    WireDerive::Debug,
    WireDerive::PartialEq,
    WireDerive::Eq,
]);

/// The admitted cell's editable context document and next-inference choices.
#[must_use]
pub fn context_read_write() -> Effect {
    Effect {
        name: "ContextReadWrite",
        authored_surface: AuthoredSurface::OPAQUE,
        handler: "ContextReadWriteDecodeHandler",
        handler_module: "context_read_write",
        req_enum: "ContextReadWriteReq",
        decl_fn: "context_read_write_decl",
        description: &[
            "Targeted transactional edits to the admitted synchronous tool's committed context projection, next model and reasoning effort. ",
            "Use `Tidepool.Agent.Context` to curate the next inference's evidence or select its model; `reflect` only reads recorded conversation data. ",
            "Accepted context batches and explicit next-model/effort choices commit together on whole-cell success; a terminal rejection rolls them back together. External effects already issued are not rolled back. ",
            "A stage refusal accepts no transaction and leaves separately requested choices and any accepted batch intact. A patch never changes model or effort; choice-only cells remain supported. ",
            "Context references and native blocks retain host-owned evidence provenance, while system and developer instructions remain host-controlled. ",
            "Saving an inspection preserves data and content versions, not authority; stage one version-bound patch and inspect its retained terminal receipt. ",
            "For curation followed by delegation, complete the synchronous context edit before capturing that context for `spawnSubagent`. Context and workspace are independent choices; see `exomonad-agent-work`. ",
            "Only an explicitly admitted synchronous profile can carry this effect; see `doc workbench` for patch and receipt rules.",
        ],
        prompt_card: None,
        type_params: &[],
        default_row_args: &[],
        extra_imports: &[],
        type_defs: type_defs(),
        external_types: &[],
        errors: None,
        verbs: vec![
            verb(
                "GetInitialContextWith",
                "get_initial_context_with",
                vec![],
                HsType::Named("InitialContextObservation"),
            ),
            verb(
                "GetContextWith",
                "get_context_with",
                vec![],
                HsType::Named("ContextInspection"),
            ),
            verb(
                "StageContextWith",
                "stage_context_with",
                vec![bridged_arg("patch", "ContextPatch", HsType::Named("ContextPatch"))],
                HsType::Named("ContextStageResult"),
            ),
            verb(
                "InspectContextTransactionWith",
                "inspect_context_transaction_with",
                vec![bridged_arg("transaction", "ContextTransaction", HsType::Named("ContextTransaction"))],
                HsType::Named("ContextTransactionStatus"),
            ),
            verb(
                "InspectOriginalTextWith",
                "inspect_original_text_with",
                vec![bridged_arg("reference", "ContextReference", HsType::Named("ContextReference")), bridged_arg("selector", "ContextTextSelector", HsType::Named("ContextTextSelector"))],
                HsType::Named("OriginalTextObservation"),
            ),
            verb(
                "SetNextModelWith",
                "set_next_model_with",
                vec![arg("model", HsType::Text)],
                HsType::Unit,
            ),
            verb(
                "SetNextEffortWith",
                "set_next_effort_with",
                vec![Arg {
                    name: "effort",
                    ty: HsType::Named("ForkEffort"),
                    rust: RustBinding::Path("crate::ForkEffort"),
                }],
                HsType::Unit,
            ),
        ],
        helpers: vec![
            Helper {
                name: "getInitialContext",
                ctor: Some("GetInitialContextWith"),
                substrate: false,
                doc: &["Observe retained startup evidence without rerunning initialization or editing context."],
                body: HelperBody::Nullary,
            },
            Helper {
                name: "getContext",
                ctor: Some("GetContextWith"),
                substrate: false,
                doc: &[],
                body: HelperBody::Nullary,
            },
            Helper {
                name: "stageContext",
                ctor: Some("StageContextWith"),
                substrate: false,
                doc: &[],
                body: HelperBody::Pointfree,
            },
            Helper {
                name: "setNextModel",
                ctor: Some("SetNextModelWith"),
                substrate: false,
                doc: &[],
                body: HelperBody::Pointfree,
            },
            Helper {
                name: "setNextEffort",
                ctor: Some("SetNextEffortWith"),
                substrate: false,
                doc: &[],
                body: HelperBody::Pointfree,
            },
        ],
        polymorphism: Polymorphism::None,
        generated_handler: false,
        handler_execution: crate::schema::HandlerExecution::BlockingPrepared,
        caller_principal: true,
    }
}

fn type_defs() -> Vec<TypeDef> {
    vec![
        record("ContextTextVersion", vec![
            field("contextTextVersionReference", "reference", HsType::Named("ContextReference")),
            field("contextTextVersionSelector", "selector", HsType::Named("ContextTextSelector")),
            field("contextTextVersionDigest", "digest", HsType::Text),
        ]),
        record("ContextUnitVersion", vec![
            field("contextUnitVersionReference", "reference", HsType::Named("ContextReference")),
            field("contextUnitVersionDigest", "digest", HsType::Text),
        ]),
        sum("ContextTextAuthor", &["ContextAuthorUser", "ContextAuthorAssistant", "ContextAuthorTool", "ContextAuthorSystem", "ContextAuthorDeveloper", "ContextAuthorUnknown"], &["Immutable canonical original occurrence role/type; Unknown remains explicit. Replacement overlays do not change this attribution; later edits retain separate transaction receipts."]),
        record("ContextTextAuthorship", vec![
            field("contextAuthorshipReference", "reference", HsType::Named("ContextReference")),
            field("contextAuthorshipSelector", "selector", HsType::Named("ContextTextSelector")),
            field("contextAuthorshipAuthor", "author", HsType::Named("ContextTextAuthor")),
            field("contextAuthorshipNote", "authored_note", HsType::Bool),
        ]),
        record("ContextInspection", vec![
            field("contextInspectionDocument", "document", HsType::Named("ContextDocument")),
            field("contextInspectionTextVersions", "text_versions", HsType::list(HsType::Named("ContextTextVersion"))),
            field("contextInspectionUnitVersions", "unit_versions", HsType::list(HsType::Named("ContextUnitVersion"))),
            field("contextInspectionAuthorship", "authorship", HsType::list(HsType::Named("ContextTextAuthorship"))),
        ]),
        record("ContextPatch", vec![field("contextPatchChanges", "changes", HsType::list(HsType::Named("ContextChange")))]),
        variants("ContextChange", vec![
            SumVariant { ctor: "ReplaceContextText", fields: VariantFields::Named(vec![
                field("contextChangeReference", "reference", HsType::Named("ContextReference")),
                field("contextChangeSelector", "selector", HsType::Named("ContextTextSelector")),
                field("contextChangeExpected", "expected", HsType::Text),
                field("contextChangeText", "text", HsType::Text),
            ]), doc: &[] },
            SumVariant { ctor: "RemoveContextUnit", fields: VariantFields::Named(vec![
                field("contextChangeReference", "reference", HsType::Named("ContextReference")),
                field("contextChangeExpected", "expected", HsType::Text),
            ]), doc: &[] },
            SumVariant { ctor: "AppendContextNote", fields: VariantFields::Named(vec![
                field("contextChangeText", "text", HsType::Text),
                field("contextChangeSources", "sources", HsType::list(HsType::Named("ContextReference"))),
            ]), doc: &[] },
        ]),
        sum("ContextProblemCode", &["ContextProblemStale", "ContextProblemMissing", "ContextProblemAmbiguous", "ContextProblemDenied", "ContextProblemInvalidPatch", "ContextProblemAlreadyStaged", "ContextProblemIneligible", "ContextProblemUnsupportedSelector"], &["Typed context patch refusal; rendered detail never controls behavior."]),
        record("ContextProblem", vec![
            field("contextProblemCode", "code", HsType::Named("ContextProblemCode")),
            field("contextProblemReference", "reference", HsType::maybe(HsType::Named("ContextReference"))),
            field("contextProblemDetail", "detail", HsType::Text),
        ]),
        TypeDef {
            name: "ContextTransaction", wire_rust: Some("ContextTransaction"), haskell_module: Some("Tidepool.Effects.Core"),
            shape: TypeShape::Identity { payload: IdentityPayload::Text, hs_binder: "transaction", rust_field: "raw", validation: Validation::None },
            json: JsonInstance::Transparent, derives: WIRE, domain: None,
            doc: &["Owner-issued transaction identifier; possession does not grant observation authority."],
        },
        record("ContextTransactionReceipt", vec![
            field("contextReceiptHead", "head", HsType::Text),
            field("contextReceiptGeneration", "generation", HsType::Int),
            field("contextReceiptChanged", "changed", HsType::Bool),
            field("contextReceiptAppended", "appended", HsType::list(HsType::Named("ContextReference"))),
        ]),
        variants("ContextTransactionStatus", vec![
            startup_variant("ContextTransactionStaged", vec![]),
            startup_variant("ContextTransactionCommitted", vec![HsType::Named("ContextTransactionReceipt")]),
            startup_variant("ContextTransactionRejected", vec![HsType::list(HsType::Named("ContextProblem"))]),
            startup_variant("ContextTransactionCancelled", vec![]),
            startup_variant("ContextTransactionUnknown", vec![HsType::Text]),
        ]),
        variants("ContextStageResult", vec![
            startup_variant("ContextStaged", vec![HsType::Named("ContextTransaction")]),
            startup_variant("ContextStageRefused", vec![HsType::list(HsType::Named("ContextProblem"))]),
        ]),
        variants("OriginalTextObservation", vec![
            startup_variant("OriginalTextAvailable", vec![HsType::Text]),
            startup_variant("OriginalTextMissing", vec![]),
            startup_variant("OriginalTextDenied", vec![HsType::Text]),
            startup_variant("OriginalTextUnavailable", vec![HsType::Text]),
            startup_variant("OriginalTextUnsupported", vec![HsType::Text]),
        ]),
        TypeDef {
            name: "InitialContextIdentity",
            wire_rust: Some("InitialContextIdentity"),
            haskell_module: Some("Tidepool.Effects.Core"),
            shape: TypeShape::Record {
                fields: vec![
                    field("initialContextOperation", "operation", HsType::Text),
                    field("initialContextActor", "actor", HsType::Tuple(vec![HsType::Int, HsType::Int])),
                    field("initialContextSpecInstall", "spec_install", HsType::Int),
                    field("initialContextSpecRevision", "spec_revision", HsType::maybe(HsType::Text)),
                    field("initialContextSpecEntry", "spec_entry", HsType::maybe(HsType::Text)),
                    field("initialContextSpecSource", "spec_source", HsType::Text),
                ],
            },
            json: JsonInstance::None,
            derives: WIRE,
            domain: None,
            doc: &["Owner-issued startup operation, actor, installed spec and exact source identity."],
        },
        TypeDef {
            name: "InitialContextPublicationReceipt",
            wire_rust: Some("InitialContextPublicationReceipt"),
            haskell_module: Some("Tidepool.Effects.Core"),
            shape: TypeShape::Record {
                fields: vec![
                    field("initialContextRun", "run", HsType::Text),
                    field("initialContextActorPath", "actor_path", HsType::Text),
                    field("initialContextConversationIncarnation", "conversation_incarnation", HsType::Text),
                    field("initialContextRequest", "request", HsType::Text),
                    field("initialContextCallerReference", "caller_reference", HsType::maybe(HsType::Named("ContextReference"))),
                ],
            },
            json: JsonInstance::None,
            derives: WIRE,
            domain: None,
            doc: &["Historical publication association and independently preserved caller seed reference."],
        },
        TypeDef {
            name: "InitialContextObservation",
            wire_rust: Some("InitialContextObservation"),
            haskell_module: Some("Tidepool.Effects.Core"),
            shape: TypeShape::Sum {
                variants: vec![
                    startup_variant("InitialContextAbsent", vec![]),
                    startup_variant("InitialContextCaptured", vec![]),
                    startup_variant("InitialContextPreparing", vec![HsType::Named("InitialContextIdentity")]),
                    startup_variant("InitialContextRetained", vec![HsType::Named("InitialContextIdentity"), HsType::Text]),
                    startup_variant("InitialContextPublishedEmpty", vec![HsType::Named("InitialContextIdentity"), HsType::Named("InitialContextPublicationReceipt")]),
                    startup_variant("InitialContextPublished", vec![HsType::Named("InitialContextIdentity"), HsType::Named("InitialContextPublicationReceipt"), HsType::Text, HsType::Named("ContextReference")]),
                    startup_variant("InitialContextFailed", vec![HsType::Named("InitialContextIdentity"), HsType::Text]),
                    startup_variant("InitialContextCancelled", vec![HsType::Named("InitialContextIdentity")]),
                    startup_variant("InitialContextUncertain", vec![HsType::Named("InitialContextIdentity"), HsType::Text]),
                ],
            },
            json: JsonInstance::None,
            derives: WIRE,
            domain: None,
            doc: &[
                "Retained startup status, with successful empty output distinct from an absent hook and failure.",
                "Published text is historical evidence; read current context before replacing its exact issued reference.",
            ],
        },
        TypeDef {
            name: "ContextReference",
            wire_rust: Some("ContextReference"),
            haskell_module: Some("Tidepool.Effects.Core"),
            shape: TypeShape::Identity {
                payload: IdentityPayload::Text,
                hs_binder: "reference",
                rust_field: "raw",
                validation: Validation::None,
            },
            json: JsonInstance::Transparent,
            derives: WIRE,
            domain: None,
            doc: &[
                "Opaque serialized reference to host-owned context evidence; the context host validates it.",
            ],
        },
        sum(
            "ContextRole",
            &["User", "Assistant"],
            &["Editable roles; system and developer instructions remain host-owned."],
        ),
        sum(
            "ContextNativeKind",
            &["CompletedExchange", "Opaque", "Pending"],
            &["Host classification of native evidence; each visible text field carries its own editability."],
        ),
        TypeDef {
            name: "ContextTextSelector",
            wire_rust: Some("ContextTextSelector"),
            haskell_module: Some("Tidepool.Effects.Core"),
            shape: TypeShape::Sum {
                variants: vec![
                    SumVariant {
                        ctor: "MessageText",
                        fields: VariantFields::Named(vec![field(
                            "contextMessagePart",
                            "part",
                            HsType::Int,
                        )]),
                        doc: &["A visible message body identified by its position in the exchange."],
                    },
                    SumVariant {
                        ctor: "ToolResultText",
                        fields: VariantFields::Positional(vec![]),
                        doc: &["The visible text of a completed tool result."],
                    },
                ],
            },
            json: JsonInstance::None,
            derives: WIRE,
            domain: None,
            doc: &["Selects one visible text field within native context evidence."],
        },
        TypeDef {
            name: "ContextVisibleText",
            wire_rust: Some("ContextVisibleText"),
            haskell_module: Some("Tidepool.Effects.Core"),
            shape: TypeShape::Record {
                fields: vec![
                    field(
                        "contextVisibleTextReference",
                        "reference",
                        HsType::Named("ContextReference"),
                    ),
                    field(
                        "contextVisibleTextSelector",
                        "selector",
                        HsType::Named("ContextTextSelector"),
                    ),
                    field("contextVisibleTextText", "text", HsType::Text),
                    field("contextVisibleTextEditable", "editable", HsType::Bool),
                    field("contextVisibleTextRemovable", "removable", HsType::Bool),
                ],
            },
            json: JsonInstance::None,
            derives: WIRE,
            domain: None,
            doc: &["A full visible native text field with its exact selector and editability."],
        },
        TypeDef {
            name: "ContextBlock",
            wire_rust: Some("ContextBlock"),
            haskell_module: Some("Tidepool.Effects.Core"),
            shape: TypeShape::Sum {
                variants: vec![
                    SumVariant {
                        ctor: "Text",
                        fields: VariantFields::Named(vec![
                            field(
                                "contextTextReference",
                                "reference",
                                HsType::maybe(HsType::Named("ContextReference")),
                            ),
                            field("contextTextRole", "role", HsType::Named("ContextRole")),
                            field("contextTextBody", "text", HsType::Text),
                            field(
                                "contextTextSources",
                                "sources",
                                HsType::list(HsType::Named("ContextReference")),
                            ),
                        ]),
                        doc: &["Editable text with its retained evidence references."],
                    },
                    SumVariant {
                        ctor: "Native",
                        fields: VariantFields::Named(vec![
                            field(
                                "contextNativeReference",
                                "reference",
                                HsType::Named("ContextReference"),
                            ),
                            field(
                                "contextNativeKind",
                                "kind",
                                HsType::Named("ContextNativeKind"),
                            ),
                            field("contextNativePreview", "preview", HsType::Text),
                            field("contextNativeProtected", "protected", HsType::Bool),
                            field(
                                "contextNativeTexts",
                                "texts",
                                HsType::list(HsType::Named("ContextVisibleText")),
                            ),
                        ]),
                        doc: &["Host-owned native evidence with bounded preview and full visible text fields."],
                    },
                ],
            },
            json: JsonInstance::None,
            derives: WIRE,
            domain: None,
            doc: &["One editable text block or host-owned native evidence block."],
        },
        TypeDef {
            name: "ContextDocument",
            wire_rust: Some("ContextDocument"),
            haskell_module: Some("Tidepool.Effects.Core"),
            shape: TypeShape::Record {
                fields: vec![field(
                    "blocks",
                    "blocks",
                    HsType::list(HsType::Named("ContextBlock")),
                )],
            },
            json: JsonInstance::None,
            derives: WIRE,
            domain: None,
            doc: &["The complete ordered context document for one admitted invocation."],
        },
    ]
}

fn startup_variant(ctor: &'static str, fields: Vec<HsType>) -> SumVariant {
    SumVariant {
        ctor,
        fields: VariantFields::Positional(fields),
        doc: &[],
    }
}

fn field(hs_name: &'static str, rust_name: &'static str, ty: HsType) -> RecordField {
    RecordField {
        hs_name,
        rust_name,
        ty,
        doc: &[],
    }
}

fn arg(name: &'static str, ty: HsType) -> Arg {
    Arg {
        name,
        ty,
        rust: RustBinding::Derived,
    }
}

fn bridged_arg(name: &'static str, wire_name: &'static str, ty: HsType) -> Arg {
    Arg {
        name,
        ty,
        rust: RustBinding::Bridged(wire_name),
    }
}

fn verb(ctor: &'static str, method: &'static str, args: Vec<Arg>, ret: HsType) -> Verb {
    Verb {
        ctor,
        method,
        args,
        ret,
        errors: None,
        handling: HandlingClass::OuterDispatch(crate::schema::OuterEffect::ContextReadWrite),
    }
}

fn sum(
    name: &'static str,
    constructors: &'static [&'static str],
    doc: &'static [&'static str],
) -> TypeDef {
    TypeDef {
        name,
        wire_rust: Some(name),
        haskell_module: Some("Tidepool.Effects.Core"),
        shape: TypeShape::Sum {
            variants: constructors
                .iter()
                .map(|ctor| SumVariant {
                    ctor,
                    fields: VariantFields::Positional(vec![]),
                    doc: &[],
                })
                .collect(),
        },
        json: JsonInstance::ShownString { binder: "value" },
        derives: WIRE,
        domain: None,
        doc,
    }
}

fn record(name: &'static str, fields: Vec<RecordField>) -> TypeDef {
    TypeDef {
        name,
        wire_rust: Some(name),
        haskell_module: Some("Tidepool.Effects.Core"),
        shape: TypeShape::Record { fields },
        json: JsonInstance::None,
        derives: WIRE,
        domain: None,
        doc: &[],
    }
}

fn variants(name: &'static str, variants: Vec<SumVariant>) -> TypeDef {
    TypeDef {
        name,
        wire_rust: Some(name),
        haskell_module: Some("Tidepool.Effects.Core"),
        shape: TypeShape::Sum { variants },
        json: JsonInstance::None,
        derives: WIRE,
        domain: None,
        doc: &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_surface_exposes_targeted_staging_and_separate_observation() {
        let effect = context_read_write();
        assert_eq!(effect.validate(), Ok(()));
        let signatures: Vec<_> = effect
            .verbs
            .iter()
            .map(|verb| (verb.ctor, &verb.ret))
            .collect();
        assert_eq!(
            signatures,
            vec![
                (
                    "GetInitialContextWith",
                    &HsType::Named("InitialContextObservation")
                ),
                ("GetContextWith", &HsType::Named("ContextInspection")),
                ("StageContextWith", &HsType::Named("ContextStageResult")),
                (
                    "InspectContextTransactionWith",
                    &HsType::Named("ContextTransactionStatus")
                ),
                (
                    "InspectOriginalTextWith",
                    &HsType::Named("OriginalTextObservation")
                ),
                ("SetNextModelWith", &HsType::Unit),
                ("SetNextEffortWith", &HsType::Unit),
            ]
        );
        let original = &effect.verbs[4];
        assert_eq!(
            original
                .args
                .iter()
                .map(|arg| (arg.name, &arg.ty))
                .collect::<Vec<_>>(),
            vec![
                ("reference", &HsType::Named("ContextReference")),
                ("selector", &HsType::Named("ContextTextSelector"))
            ]
        );
    }

    #[test]
    fn inspection_keeps_content_once_and_patch_requires_versions() {
        let definitions = type_defs();
        let fields = |name| match &definitions
            .iter()
            .find(|definition| definition.name == name)
            .unwrap()
            .shape
        {
            TypeShape::Record { fields } => fields
                .iter()
                .map(|field| (field.rust_name, field.ty.clone()))
                .collect::<Vec<_>>(),
            _ => panic!("expected record"),
        };
        assert_eq!(
            fields("ContextInspection"),
            vec![
                ("document", HsType::Named("ContextDocument")),
                (
                    "text_versions",
                    HsType::list(HsType::Named("ContextTextVersion"))
                ),
                (
                    "unit_versions",
                    HsType::list(HsType::Named("ContextUnitVersion"))
                ),
                (
                    "authorship",
                    HsType::list(HsType::Named("ContextTextAuthorship"))
                ),
            ]
        );
        assert_eq!(
            fields("ContextTextVersion"),
            vec![
                ("reference", HsType::Named("ContextReference")),
                ("selector", HsType::Named("ContextTextSelector")),
                ("digest", HsType::Text),
            ]
        );
        assert_eq!(
            fields("ContextTextAuthorship"),
            vec![
                ("reference", HsType::Named("ContextReference")),
                ("selector", HsType::Named("ContextTextSelector")),
                ("author", HsType::Named("ContextTextAuthor")),
                ("authored_note", HsType::Bool),
            ]
        );
        let change = definitions
            .iter()
            .find(|definition| definition.name == "ContextChange")
            .unwrap();
        let TypeShape::Sum { variants } = &change.shape else {
            panic!("expected patch sum")
        };
        let VariantFields::Named(replacement) = &variants[0].fields else {
            panic!("expected named replacement")
        };
        assert_eq!(
            replacement
                .iter()
                .map(|field| field.rust_name)
                .collect::<Vec<_>>(),
            vec!["reference", "selector", "expected", "text"]
        );
        let VariantFields::Named(note) = &variants[2].fields else {
            panic!("expected named note")
        };
        assert_eq!(
            note.iter().map(|field| field.rust_name).collect::<Vec<_>>(),
            vec!["text", "sources"]
        );
    }
}
