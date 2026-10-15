//! Independent idle subagent admission and compiled installation replacement.

use crate::hs::HsType;
use crate::schema::{
    Arg, Effect, HandlingClass, JsonInstance, Polymorphism, RecordField, RustBinding, SumVariant,
    TypeDef, TypeShape, VariantFields, Verb, WireDerive, WireDerives,
};

/// Admit an idle persistent actor with explicit context, directory and tools.
#[must_use]
pub fn agent_launch() -> Effect {
    Effect {
        name: "AgentLaunch",
        authored_surface: crate::schema::AuthoredSurface::OPAQUE,
        handler: "AgentLaunchDecodeHandler",
        handler_module: "agent_launch",
        req_enum: "AgentLaunchReq",
        decl_fn: "agent_launch_decl",
        description: &[
            "Independent idle subagent admission with compiled tools and explicit context, workspace and cleanup ownership.",
        ],
        prompt_card: None,
        type_params: &[],
        default_row_args: &[],
        extra_imports: &[],
        type_defs: spawn_types(),
        external_types: &[
            crate::schema::ExternalType {
                haskell_name: "WorkspaceHandle",
                rust_wire: "tidepool_bridge_effects::WtWorkspaceHandle",
                core_module: None,
            },
            crate::schema::ExternalType {
                haskell_name: "WorktreeSource",
                rust_wire: "tidepool_bridge_effects::WtWorktreeSource",
                core_module: None,
            },
            crate::schema::ExternalType {
                haskell_name: "WorktreeHandle",
                rust_wire: "tidepool_bridge_effects::WtWorktreeHandle",
                core_module: None,
            },
            crate::schema::ExternalType {
                haskell_name: "Scope",
                rust_wire: "tidepool_bridge_effects::ResourceScopeId",
                core_module: None,
            },
        ],
        errors: None,
        verbs: vec![
            Verb {
                ctor: "AgentLaunchSpawnWith",
                method: "agent_launch_spawn_with",
                args: vec![
                    Arg {
                        name: "context",
                        ty: HsType::Named("SpawnContextWire"),
                        rust: RustBinding::Path("crate::start::SpawnContextWire"),
                    },
                    installer_arg(),
                    Arg {
                        name: "workspace",
                        ty: HsType::Named("SpawnWorkspaceWire"),
                        rust: RustBinding::Path("crate::fork_workspace::SpawnWorkspaceWire"),
                    },
                    Arg {
                        name: "effects",
                        ty: HsType::list(HsType::Named("ActorEffectKey")),
                        rust: RustBinding::Path("Vec<crate::ActorEffectKeyWire>"),
                    },
                    optional_arg("label", HsType::Text, RustBinding::Derived),
                    optional_arg(
                        "model",
                        HsType::Named("Model"),
                        RustBinding::Path("Option<crate::Model>"),
                    ),
                    optional_arg(
                        "effort",
                        HsType::Named("ForkEffort"),
                        RustBinding::Path("Option<crate::ForkEffort>"),
                    ),
                    optional_arg("instructions", HsType::Text, RustBinding::Derived),
                    Arg {
                        name: "lifetime",
                        ty: HsType::Named("WorkerLifetime"),
                        rust: RustBinding::Path("crate::WorkerLifetime"),
                    },
                    optional_arg(
                        "limits",
                        HsType::Tuple(vec![HsType::Int, HsType::Int]),
                        RustBinding::Path("Option<(i64, i64)>"),
                    ),
                    Arg { name: "spec_metadata", ty: HsType::Value, rust: RustBinding::HaskellValue },
                ],
                ret: HsType::either(
                    HsType::Named("SpawnErrorWire"),
                    HsType::Tuple(vec![
                        HsType::Int,
                        HsType::Int,
                        HsType::maybe(HsType::Named("WorktreeHandle")),
                    ]),
                ),
                errors: None,
                handling: HandlingClass::Actor,
            },
            Verb {
                ctor: "AgentLaunchValidateSpecWith",
                method: "agent_launch_validate_spec_with",
                args: vec![
                    Arg { name: "spec_metadata", ty: HsType::Value, rust: RustBinding::HaskellValue },
                    Arg { name: "effects", ty: HsType::list(HsType::Named("ActorEffectKey")), rust: RustBinding::Path("Vec<crate::ActorEffectKeyWire>") },
                ],
                ret: HsType::either(HsType::Named("SpecValidationError"), HsType::Named("SpecValidationReceipt")),
                errors: None,
                handling: HandlingClass::Actor,
            },
            Verb {
                ctor: "AgentLaunchReplaceSpecWith",
                method: "agent_launch_replace_spec_with",
                args: vec![
                    Arg {
                        name: "target",
                        ty: HsType::Tuple(vec![HsType::Int, HsType::Int]),
                        rust: RustBinding::Path("(i64, i64)"),
                    },
                    installer_arg(),
                    Arg {
                        name: "effects",
                        ty: HsType::list(HsType::Named("ActorEffectKey")),
                        rust: RustBinding::Path("Vec<crate::ActorEffectKeyWire>"),
                    },
                ],
                ret: HsType::either(HsType::Named("SpecReplacementError"), HsType::Unit),
                errors: None,
                handling: HandlingClass::Actor,
            },
            Verb {
                ctor: "AgentLaunchCheckpointWith",
                method: "agent_launch_checkpoint_with",
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
                ctor: "AgentLaunchCheckCheckpointWith",
                method: "agent_launch_check_checkpoint_with",
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
                ctor: "AgentLaunchReleaseCheckpointWith",
                method: "agent_launch_release_checkpoint_with",
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

fn installer_arg() -> Arg {
    Arg {
        name: "install",
        ty: HsType::func(
            HsType::Int,
            HsType::app(
                HsType::app(HsType::Named("Eff"), HsType::Var("childEffs")),
                HsType::Unit,
            ),
        ),
        rust: RustBinding::HaskellValue,
    }
}

fn optional_arg(name: &'static str, ty: HsType, rust: RustBinding) -> Arg {
    Arg {
        name,
        ty: HsType::maybe(ty),
        rust,
    }
}

fn sum_type(
    name: &'static str,
    wire_rust: Option<&'static str>,
    variants: Vec<(&'static str, Vec<HsType>)>,
) -> TypeDef {
    TypeDef {
        name,
        wire_rust,
        haskell_module: None,
        shape: TypeShape::Sum {
            variants: variants
                .into_iter()
                .map(|(ctor, fields)| SumVariant {
                    ctor,
                    fields: VariantFields::Positional(fields),
                    doc: &[],
                })
                .collect(),
        },
        json: JsonInstance::None,
        derives: WireDerives(&[]),
        domain: None,
        doc: &[],
    }
}

fn spec_type(
    name: &'static str,
    rust: &'static str,
    variants: Vec<(&'static str, Vec<HsType>)>,
) -> TypeDef {
    let mut definition = sum_type(name, Some(rust), variants);
    definition.json = JsonInstance::Generic;
    definition
}

fn named_spec_type(
    name: &'static str,
    rust: &'static str,
    ctor: &'static str,
    fields: Vec<(&'static str, HsType)>,
) -> TypeDef {
    let mut definition = spec_type(name, rust, vec![]);
    definition.shape = TypeShape::Sum {
        variants: vec![SumVariant {
            ctor,
            fields: VariantFields::Named(
                fields
                    .into_iter()
                    .map(|(field, ty)| RecordField {
                        hs_name: field,
                        rust_name: field,
                        ty,
                        doc: &[],
                    })
                    .collect(),
            ),
            doc: &[],
        }],
    };
    definition
}

fn spawn_types() -> Vec<TypeDef> {
    let mut types = context_types();
    types.extend(vec![
        spec_type(
            "SpecValidationError",
            "crate::tool_contract::SpecValidationError",
            vec![
                ("SpecPreparationFailed", vec![HsType::Text]),
                ("SpecMetadataInvalid", vec![HsType::Text]),
                ("SpecEffectMismatch", vec![HsType::Text]),
                ("SpecInterpreterUnavailable", vec![HsType::Text]),
                ("SpecInputSchemaInvalid", vec![HsType::Text]),
                ("SpecProviderProjectionInvalid", vec![HsType::Text]),
            ],
        ),
        named_spec_type(
            "SpecProvenance",
            "crate::tool_contract::SpecProvenance",
            "SpecProvenanceUnavailable",
            vec![("specProvenanceReason", HsType::Text)],
        ),
        spec_type(
            "SpecProviderValidation",
            "crate::tool_contract::SpecProviderValidation",
            vec![
                ("SpecProviderNotChecked", vec![]),
                ("SpecProviderChecked", vec![]),
            ],
        ),
        named_spec_type(
            "SpecValidationReceipt",
            "crate::tool_contract::SpecValidationReceipt",
            "SpecValidationReceipt",
            vec![
                ("specValidatedEndpoints", HsType::list(HsType::Text)),
                ("specRequestedEffects", HsType::list(HsType::Text)),
                ("specProvenance", HsType::Named("SpecProvenance")),
                (
                    "specProviderValidation",
                    HsType::Named("SpecProviderValidation"),
                ),
            ],
        ),
        sum_type(
            "SpawnContextWire",
            Some("crate::start::SpawnContextWire"),
            vec![
                ("CapturedSpawn", vec![HsType::Text]),
                ("FreshSpawn", vec![HsType::Text]),
            ],
        ),
        sum_type(
            "WorkspaceSeedWire",
            Some("crate::fork_workspace::WorkspaceSeedWire"),
            vec![
                ("CurrentCheckout", vec![]),
                ("CommittedSource", vec![HsType::Named("WorktreeSource")]),
            ],
        ),
        sum_type(
            "SpawnWorkspaceWire",
            Some("crate::fork_workspace::SpawnWorkspaceWire"),
            vec![
                ("SameDirectory", vec![]),
                ("ExistingDirectory", vec![HsType::Named("WorkspaceHandle")]),
                ("ForkDirectory", vec![HsType::Named("WorkspaceSeedWire")]),
            ],
        ),
        sum_type(
            "SpawnRetainedResourcesWire",
            None,
            vec![
                (
                    "SpawnRetainedWorkspace",
                    vec![HsType::Named("WorktreeHandle")],
                ),
                (
                    "SpawnRetainedActor",
                    vec![
                        HsType::Tuple(vec![HsType::Int, HsType::Int]),
                        HsType::maybe(HsType::Named("WorktreeHandle")),
                    ],
                ),
            ],
        ),
        sum_type(
            "SpawnCleanup",
            None,
            vec![
                ("SpawnCleanupNotNeeded", vec![]),
                ("SpawnCleanupConfirmed", vec![]),
                ("SpawnCleanupUnconfirmed", vec![HsType::Text]),
            ],
        ),
        sum_type(
            "SpawnErrorWire",
            None,
            vec![
                ("SpawnRefused", vec![HsType::Text]),
                (
                    "SpawnPartialFailure",
                    vec![
                        HsType::Named("SpawnRetainedResourcesWire"),
                        HsType::Named("SpawnCleanup"),
                        HsType::Text,
                    ],
                ),
            ],
        ),
        sum_type(
            "SpecReplacementError",
            None,
            vec![
                ("SpecReplacementUnavailable", vec![]),
                ("SpecReplacementUnauthorized", vec![]),
                ("SpecReplacementSurfaceChanged", vec![]),
                ("SpecReplacementFailed", vec![HsType::Text]),
            ],
        ),
    ]);
    types
}

fn context_types() -> Vec<TypeDef> {
    vec![
        TypeDef {
            name: "Model",
            wire_rust: Some("Model"),
            haskell_module: Some("Tidepool.Effects.Core"),
            shape: TypeShape::Sum {
                variants: vec![
                    SumVariant {
                        ctor: "Luna",
                        fields: VariantFields::Positional(vec![]),
                        doc: &["The configured Luna tier."],
                    },
                    SumVariant {
                        ctor: "Sol",
                        fields: VariantFields::Positional(vec![]),
                        doc: &["The configured Sol tier."],
                    },
                    SumVariant {
                        ctor: "Astra",
                        fields: VariantFields::Positional(vec![]),
                        doc: &["The configured Astra tier."],
                    },
                    SumVariant {
                        ctor: "CustomAlias",
                        fields: VariantFields::Positional(vec![HsType::Text]),
                        doc: &[],
                    },
                    SumVariant {
                        ctor: "ProviderModel",
                        fields: VariantFields::Positional(vec![HsType::Text]),
                        doc: &[],
                    },
                ],
            },
            json: JsonInstance::None,
            derives: WireDerives(&[
                WireDerive::FromHaskell,
                WireDerive::ToHaskell,
                WireDerive::Clone,
                WireDerive::Debug,
                WireDerive::PartialEq,
                WireDerive::Eq,
            ]),
            domain: None,
            doc: &[
                "A configured built-in tier, custom workspace alias, or explicit provider model name.",
            ],
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
        crate::actor_profiles::actor_key_definition(),
    ]
}
