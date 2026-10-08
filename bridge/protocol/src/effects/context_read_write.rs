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
            "Transactional editing of the admitted synchronous tool's typed context document, next model and reasoning effort. ",
            "Use `Tidepool.Agent.Context` to curate the next inference's evidence or select its model; `reflect` only reads recorded conversation data. ",
            "Context edits stage together and commit on whole-cell success; external effects already issued are not rolled back on failure. ",
            "Context references and native blocks retain host-owned evidence provenance, while system and developer instructions remain host-controlled. ",
            "Saving a context value preserves data, not future edit authority; restoration must pass the current protected-structure checks. ",
            "For curation followed by delegation, complete the synchronous context edit before capturing that context for `spawnSubagent`. Context and workspace are independent choices; see `exomonad-agent-work`. ",
            "Only an explicitly admitted synchronous profile can carry this effect; see `doc workbench` for edit and restore rules.",
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
                "GetContextWith",
                "get_context_with",
                vec![],
                HsType::Named("ContextDocument"),
            ),
            verb(
                "PutContextWith",
                "put_context_with",
                vec![bridged_arg(
                    "document",
                    "ContextDocument",
                    HsType::Named("ContextDocument"),
                )],
                HsType::Unit,
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
                name: "getContext",
                ctor: Some("GetContextWith"),
                substrate: false,
                doc: &[],
                body: HelperBody::Nullary,
            },
            Helper {
                name: "putContext",
                ctor: Some("PutContextWith"),
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
        TypeDef {
            name: "ContextReference",
            wire_rust: Some("ContextReference"),
            haskell_module: None,
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
            haskell_module: None,
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
            haskell_module: None,
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
            haskell_module: None,
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
            haskell_module: None,
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
        haskell_module: None,
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
