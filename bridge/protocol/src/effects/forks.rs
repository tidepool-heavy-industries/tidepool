//! Cache-preserving actor-fork admission capability.

use crate::hs::HsType;
use crate::schema::{
    Arg, Effect, HandlingClass, JsonInstance, Polymorphism, RustBinding, SumVariant,
    TypeDef, TypeShape, VariantFields, Verb, WireDerives,
};


#[must_use]
pub fn forks() -> Effect {
    Effect {
        name: "Forks",
        authored_surface: crate::schema::AuthoredSurface::OPAQUE,
        handler: "ForksDecodeHandler",
        handler_module: "forks",
        req_enum: "ForksReq",
        decl_fn: "forks_decl",
        description: &["Capture and release exact provider and Haskell context checkpoints for independent subagent admission."],
        prompt_card: None,
        type_params: &[],
        default_row_args: &[],
        extra_imports: &[],
        type_defs: vec![
            TypeDef {
                name: "Model",
                wire_rust: None,
                haskell_module: None,
                shape: TypeShape::Sum {
                    variants: vec![
                        SumVariant {
                            ctor: "Alias",
                            fields: VariantFields::Positional(vec![HsType::Text]),
                            doc: &[],
                        },
                        SumVariant {
                            ctor: "Literal",
                            fields: VariantFields::Positional(vec![HsType::Text]),
                            doc: &[],
                        },
                    ],
                },
                json: JsonInstance::None,
                derives: WireDerives(&[]),
                domain: None,
                doc: &["A frozen workspace alias or an explicit provider model name."],
            },
            TypeDef {
                name: "WorkerLifetime",
                wire_rust: None,
                haskell_module: None,
                shape: TypeShape::Sum {
                    variants: ["InvocationOwned", "ActorOwned", "RunOwned"]
                        .into_iter()
                        .map(|ctor| SumVariant {
                            ctor,
                            fields: VariantFields::Positional(Vec::new()),
                            doc: &[],
                        })
                        .chain(std::iter::once(SumVariant {
                            ctor: "InScope",
                            fields: VariantFields::Positional(vec![HsType::Named("Scope")]),
                            doc: &[],
                        }))
                        .collect(),
                },
                json: JsonInstance::None,
                derives: WireDerives(&[]),
                domain: None,
                doc: &[
                    "Lifetime selects cleanup ownership independently of authority and construction provenance. InScope explicitly selects a runtime-issued lexical scope; returning a handle does not transfer lifetime.",
                ],
            },
            TypeDef {
                name: "CheckpointRefusal",
                wire_rust: None,
                haskell_module: None,
                shape: TypeShape::Sum {
                    variants: [
                        "NoHostedBoundary",
                        "WrongSession",
                        "UnavailableCheckpoint",
                        "ReleasedCheckpoint",
                        "CaptureFailed",
                        "ProcessRestartUnsupported",
                    ]
                    .into_iter()
                    .map(|ctor| SumVariant {
                        ctor,
                        fields: VariantFields::Positional(Vec::new()),
                        doc: &[],
                    })
                    .collect(),
                },
                json: JsonInstance::None,
                derives: WireDerives(&[]),
                domain: None,
                doc: &["Why an exact hosted context checkpoint cannot be captured or used."],
            },
            TypeDef {
                name: "ForkEffort",
                wire_rust: None,
                haskell_module: None,
                shape: TypeShape::Sum {
                    variants: ["Low", "Medium", "High"]
                        .into_iter()
                        .map(|ctor| SumVariant {
                            ctor,
                            fields: VariantFields::Positional(Vec::new()),
                            doc: &[],
                        })
                        .collect(),
                },
                json: JsonInstance::None,
                derives: WireDerives(&[]),
                domain: None,
                doc: &["Reasoning effort selected before a context child's first inference."],
            },
            TypeDef {
                name: "ActorEffectKey",
                wire_rust: None,
                haskell_module: None,
                shape: TypeShape::Sum {
                    variants: [
                        "EffectResourceScopes",
                        "EffectReplies",
                        "EffectWatches",
                        "EffectForks",
                        "EffectActorContext",
                        "EffectAgentLaunch",
                        "EffectAgentInspection",
                        "EffectAgentControl",
                        "EffectBoundWorktree",
                        "EffectWorktreeRegistry",
                        "EffectWorktreeAllocation",
                        "EffectWorktreeIntegration",
                        "EffectSleep",
                        "EffectCommands",
                        "EffectConsole",
                        "EffectNotifications",
                        "EffectJev",
                        "EffectModelCall",
                        "EffectActor",
                        "EffectReflect",
                        "EffectLookup",
                        "EffectRepoEvent",
                        "EffectSource",
                        "EffectJournal",
                    ]
                    .into_iter()
                    .map(|ctor| SumVariant {
                        ctor,
                        fields: VariantFields::Positional(Vec::new()),
                        doc: &[],
                    })
                    .collect(),
                },
                json: JsonInstance::None,
                derives: WireDerives(&[]),
                domain: None,
                doc: &["Stable nominal key for one generated model-facing effect."],
            },
        ],
        external_types: &[
            crate::schema::ExternalType { haskell_name: "Scope", rust_wire: "tidepool_bridge_effects::ResourceScopeId", core_module: None },
        ],
        errors: None,
        verbs: vec![
            Verb {
                ctor: "ForksCheckpointWith",
                method: "forks_checkpoint_with",
                args: vec![Arg {
                    name: "name",
                    ty: HsType::Text,
                    rust: RustBinding::Path("String"),
                }],
                ret: HsType::either(HsType::Named("CheckpointRefusal"), HsType::Text),
                errors: None,
                handling: HandlingClass::Actor,
            },
            Verb {
                ctor: "ForksCheckCheckpointWith",
                method: "forks_check_checkpoint_with",
                args: vec![Arg {
                    name: "checkpoint",
                    ty: HsType::Text,
                    rust: RustBinding::Path("String"),
                }],
                ret: HsType::either(HsType::Named("CheckpointRefusal"), HsType::Unit),
                errors: None,
                handling: HandlingClass::Actor,
            },
            Verb {
                ctor: "ForksReleaseCheckpointWith",
                method: "forks_release_checkpoint_with",
                args: vec![Arg {
                    name: "checkpoint",
                    ty: HsType::Text,
                    rust: RustBinding::Path("String"),
                }],
                ret: HsType::either(HsType::Named("CheckpointRefusal"), HsType::Unit),
                errors: None,
                handling: HandlingClass::Actor,
            },
        ],
        helpers: Vec::new(),
        polymorphism: Polymorphism::None,
        generated_handler: false,
        handler_execution: crate::schema::HandlerExecution::Immediate,
        caller_principal: false,
    }
}
