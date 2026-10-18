//! Independent idle subagent admission and compiled installation replacement.

use crate::hs::HsType;
use crate::schema::{
    Arg, Effect, HandlingClass, JsonInstance, Polymorphism, RecordField, RustBinding, SumVariant,
    TypeDef, TypeShape, VariantFields, Verb, WireDerive, WireDerives,
};

pub(crate) const MODEL_FAMILY_PARAMS: &[crate::schema::TypeParam] =
    &[crate::schema::TypeParam::value("modelFamily")];

/// Validate or replace installations and inspect or release checkpoints.
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
            "Validate or replace compiled agent tools and inspect or release retained checkpoints.",
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
                ctor: "AgentLaunchValidateSpecWith",
                method: "agent_launch_validate_spec_with",
                args: vec![
                    Arg {
                        name: "spec_metadata",
                        ty: HsType::Value,
                        rust: RustBinding::HaskellValue,
                    },
                    Arg {
                        name: "effects",
                        ty: HsType::list(HsType::Named("ActorEffectKey")),
                        rust: RustBinding::Path("Vec<crate::ActorEffectKeyWire>"),
                    },
                ],
                ret: HsType::either(
                    HsType::Named("SpecValidationError"),
                    HsType::Named("SpecValidationReceipt"),
                ),
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

pub(crate) fn spawn_types() -> Vec<TypeDef> {
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

fn inference_type(name: &'static str, variants: Vec<(&'static str, Vec<HsType>)>) -> TypeDef {
    let mut definition = sum_type(name, Some(name), variants);
    definition.haskell_module = Some("Tidepool.Effects.Core");
    definition.json = JsonInstance::Generic;
    definition.derives = WireDerives(&[
        WireDerive::FromHaskell,
        WireDerive::ToHaskell,
        WireDerive::Clone,
        WireDerive::Debug,
        WireDerive::PartialEq,
        WireDerive::Eq,
        WireDerive::Serialize,
        WireDerive::Deserialize,
    ]);
    if name != "Model" {
        definition.derives = WireDerives(&[
            WireDerive::FromHaskell,
            WireDerive::ToHaskell,
            WireDerive::Clone,
            WireDerive::Copy,
            WireDerive::Debug,
            WireDerive::PartialEq,
            WireDerive::Eq,
            WireDerive::Serialize,
            WireDerive::Deserialize,
        ]);
    }
    definition
}

fn context_types() -> Vec<TypeDef> {
    vec![
        inference_type("Backend", vec![("DeepSeek", vec![]), ("OpenAIPlan", vec![]), ("GeminiInteractions", vec![])]),
        inference_type("OpenAIModel", vec![
            ("Gpt61Sol", vec![]), ("Gpt6Sol", vec![]), ("Gpt6Astra", vec![]),
            ("Gpt6Luna", vec![]), ("Gpt56Sol", vec![]),
        ]),
        inference_type("Model", vec![
            ("DeepSeekFlash", vec![]), ("DeepSeekPro", vec![]), ("GeminiArgon", vec![]),
            ("OpenAIModel", vec![HsType::Named("OpenAIModel")]),
            ("CustomModel", vec![HsType::Named("Backend"), HsType::Text]),
        ]),
        inference_type("DeepSeekReasoning", vec![
            ("DeepSeekNone", vec![]), ("DeepSeekLow", vec![]),
            ("DeepSeekHigh", vec![]), ("DeepSeekMax", vec![]),
        ]),
        inference_type("OpenAIReasoning", vec![
            ("OpenAILow", vec![]), ("OpenAIMedium", vec![]), ("OpenAIHigh", vec![]),
        ]),
        inference_type("ArgonEffort", vec![
            ("EffortLow", vec![]), ("EffortMedium", vec![]), ("EffortHigh", vec![]),
        ]),
        inference_type("Reasoning", vec![
            ("DeepSeekReasoning", vec![HsType::Named("DeepSeekReasoning")]),
            ("OpenAIReasoning", vec![HsType::Named("OpenAIReasoning")]),
            ("ArgonReasoning", vec![HsType::Named("ArgonEffort")]),
        ]),
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
        crate::actor_profiles::actor_key_definition(),
    ]
}

/// Fresh-conversation admission, qualified by the selected model family.
pub fn spawn_agent() -> Effect {
    let mut effect = agent_launch();
    effect.name = "SpawnAgent";
    effect.description = &["Admit an idle agent with a fresh conversation, explicit model selection, tools, workspace and child model grants."];
    effect.req_enum = "SpawnAgentReq";
    effect.decl_fn = "spawn_agent_decl";
    effect.type_params = MODEL_FAMILY_PARAMS;
    effect.default_row_args = &["OpenAI"];
    effect.type_defs.clear();
    effect.verbs = vec![Verb {
        ctor: "SpawnAgentSpawnWith",
        method: "spawn_agent_spawn_with",
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
            Arg {
                name: "modelGrants",
                ty: HsType::Value,
                rust: RustBinding::JsonValue,
            },
            optional_arg("label", HsType::Text, RustBinding::Derived),
            Arg {
                name: "selection",
                ty: HsType::Value,
                rust: RustBinding::JsonValue,
            },
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
            Arg {
                name: "spec_metadata",
                ty: HsType::Value,
                rust: RustBinding::HaskellValue,
            },
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
    }];
    for verb in &mut effect.verbs {
        verb.args.insert(0, family_witness_arg());
    }
    effect
}

/// Captured-conversation admission and checkpoint issuance for one model family.
pub fn fork_agent() -> Effect {
    let mut effect = spawn_agent();
    effect.name = "ForkAgent";
    effect.description = &["Capture a conversation checkpoint or admit an idle agent from captured context with explicit model selection and child model grants."];
    effect.req_enum = "ForkAgentReq";
    effect.decl_fn = "fork_agent_decl";
    effect.verbs[0].ctor = "ForkAgentSpawnWith";
    effect.verbs[0].method = "fork_agent_spawn_with";
    effect.verbs.push(Verb {
        ctor: "ForkAgentCheckpointWith",
        method: "fork_agent_checkpoint_with",
        args: vec![
            family_witness_arg(),
            Arg {
                name: "name",
                ty: HsType::Text,
                rust: RustBinding::Path("String"),
            },
        ],
        ret: HsType::either(HsType::Named("CheckpointRefusal"), HsType::Text),
        errors: None,
        handling: HandlingClass::Actor,
    });
    effect
}

pub(crate) fn family_witness_arg() -> Arg {
    Arg {
        name: "family",
        ty: HsType::app(HsType::Named("FamilyWitness"), HsType::Var("modelFamily")),
        rust: RustBinding::Path("tidepool_bridge_effects::ModelFamilyWitness"),
    }
}
