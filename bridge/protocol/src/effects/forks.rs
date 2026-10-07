//! Cache-preserving actor-fork admission capability.

use crate::hs::HsType;
use crate::schema::{
    Arg, Effect, HandlingClass, JsonInstance, Polymorphism, RecordField, RustBinding, SumVariant,
    TypeDef, TypeShape, VariantFields, Verb, WireDerives,
};

use super::agent_launch::launch_args;

#[must_use]
pub fn forks() -> Effect {
    Effect {
        name: "Forks",
        authored_surface: crate::schema::AuthoredSurface::OPAQUE,
        handler: "ForksDecodeHandler",
        handler_module: "forks",
        req_enum: "ForksReq",
        decl_fn: "forks_decl",
        description: &[
            "Compose typed agent computations through applicative `unfold`: local assignment and reply types make agent RPC part of ordinary Haskell dataflow. ",
            "Construct independent branches as values, combine heterogeneous results into a product, and retain their typed responses for continuations or actor events. ",
            "A branch whose assignment depends on another child's result belongs in a later monadic stage; adding `<*>` cannot remove that dependency. ",
            "Immediate admission needs `selected` or `fromCheckpoint` context; invocation ownership is the default. ",
            "Captured context is a snapshot, checkout selection chooses source, and lifetime determines which owner keeps unfinished work alive. ",
            "Use `unfoldDeferred` with explicit persistent lifetime after synchronous context curation so children start from the committed invocation; return before awaiting those children. ",
            "Join with `Await`/`waitFor`, register a watch for later observation, or feed results into an actor that interprets your orchestration language. Read `exomonad-unfold` for primitives and `exomonad-fork` for Project compositions. ",
            "The effect constructors are private admission substrate.",
        ],
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
                name: "WorkerLaunchPreview",
                wire_rust: None,
                haskell_module: None,
                shape: TypeShape::Record {
                    fields: vec![
                        RecordField {
                            hs_name: "launchModel",
                            rust_name: "launchModel",
                            ty: HsType::maybe(HsType::Text),
                            doc: &[],
                        },
                        RecordField {
                            hs_name: "launchEffort",
                            rust_name: "launchEffort",
                            ty: HsType::Named("ForkEffort"),
                            doc: &[],
                        },
                        RecordField {
                            hs_name: "launchInstructions",
                            rust_name: "launchInstructions",
                            ty: HsType::Text,
                            doc: &[],
                        },
                        RecordField {
                            hs_name: "launchBaseFingerprint",
                            rust_name: "launchBaseFingerprint",
                            ty: HsType::Text,
                            doc: &[],
                        },
                        RecordField {
                            hs_name: "launchWorkspaceIdentity",
                            rust_name: "launchWorkspaceIdentity",
                            ty: HsType::maybe(HsType::Text),
                            doc: &[],
                        },
                        RecordField {
                            hs_name: "launchModules",
                            rust_name: "launchModules",
                            ty: HsType::list(HsType::Text),
                            doc: &[],
                        },
                    ],
                },
                json: JsonInstance::None,
                derives: WireDerives(&[]),
                domain: None,
                doc: &[
                    "Resolved host settings. Nothing model preserves the parent's boundary selection; paths and request orientation are added at admission.",
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
                name: "ForkContext",
                wire_rust: None,
                haskell_module: None,
                shape: TypeShape::Sum {
                    variants: ["InheritedContext", "SelectedContext"]
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
                doc: &["Whether a child inherits the completed provider and Haskell context."],
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
            TypeDef {
                name: "ForkGroupCleanupOutcome",
                wire_rust: None,
                haskell_module: None,
                shape: TypeShape::Sum {
                    variants: vec![
                        SumVariant {
                            ctor: "ForkGroupCleaned",
                            fields: VariantFields::Positional(Vec::new()),
                            doc: &[],
                        },
                        SumVariant {
                            ctor: "ForkGroupStillActive",
                            fields: VariantFields::Positional(vec![HsType::List(Box::new(
                                HsType::Tuple(vec![HsType::Int, HsType::Int]),
                            ))]),
                            doc: &[],
                        },
                        SumVariant {
                            ctor: "ForkGroupCleanupRejected",
                            fields: VariantFields::Positional(vec![HsType::Text]),
                            doc: &[],
                        },
                    ],
                },
                json: JsonInstance::None,
                derives: WireDerives(&[]),
                domain: None,
                doc: &["Refusal-bearing release of one committed fork group's scheduler metadata."],
            },
        ],
        external_types: &[
            crate::schema::ExternalType { haskell_name: "Scope", rust_wire: "tidepool_bridge_effects::ResourceScopeId", core_module: None },
            crate::schema::ExternalType { haskell_name: "ActorLaunchRole", rust_wire: "crate::ActorLaunchRoleWire", core_module: None },
            crate::schema::ExternalType { haskell_name: "ActorEffectProfile", rust_wire: "crate::ActorEffectProfileWire", core_module: None },
            crate::schema::ExternalType { haskell_name: "WorktreeSpec", rust_wire: "tidepool_bridge_effects::WtWorktreeSpec", core_module: None },
            crate::schema::ExternalType { haskell_name: "DirtyPolicy", rust_wire: "tidepool_bridge_effects::WtDirtyPolicy", core_module: None },
            crate::schema::ExternalType { haskell_name: "WorktreeHandle", rust_wire: "tidepool_bridge_effects::WtWorktreeHandle", core_module: None },
        ],
        errors: None,
        verbs: vec![
            Verb {
                ctor: "ForksBeginWith",
                method: "forks_begin_with",
                args: vec![
                    Arg {
                        name: "relative",
                        ty: HsType::Bool,
                        rust: RustBinding::Derived,
                    },
                    Arg {
                        name: "group",
                        ty: HsType::Text,
                        rust: RustBinding::Derived,
                    },
                    Arg {
                        name: "branches",
                        ty: HsType::List(Box::new(HsType::Text)),
                        rust: RustBinding::Derived,
                    },
                ],
                ret: fallible(HsType::Tuple(vec![
                    HsType::Int,
                    HsType::Text,
                    HsType::List(Box::new(HsType::Text)),
                ])),
                errors: None,
                handling: HandlingClass::Actor,
            },
            {
                let mut args = launch_args(true);
                args.push(Arg {
                    name: "worktreeSpec",
                    ty: HsType::maybe(HsType::Named("WorktreeSpec")),
                    rust: RustBinding::External,
                });
                args.push(Arg {
                    name: "boundDirtyPolicy",
                    ty: HsType::Named("DirtyPolicy"),
                    rust: RustBinding::External,
                });
                args.push(Arg {
                    name: "effectKeys",
                    ty: HsType::list(HsType::Named("ActorEffectKey")),
                    rust: RustBinding::Path("Vec<crate::ActorEffectKeyWire>"),
                });
                args.push(Arg {
                    name: "effort",
                    ty: HsType::maybe(HsType::Named("ForkEffort")),
                    rust: RustBinding::Path("Option<crate::ForkEffort>"),
                });
                args.push(Arg {
                    name: "budget",
                    ty: HsType::maybe(HsType::Tuple(vec![HsType::Int, HsType::Int])),
                    rust: RustBinding::Path("Option<(i64, i64)>"),
                });
                args.push(Arg {
                    name: "model",
                    ty: HsType::maybe(HsType::Named("Model")),
                    rust: RustBinding::Path("Option<crate::Model>"),
                });
                args.push(Arg {
                    name: "context",
                    ty: HsType::Named("ForkContext"),
                    rust: RustBinding::Path("crate::ForkContext"),
                });
                args.push(Arg {
                    name: "checkpoint",
                    ty: HsType::maybe(HsType::Text),
                    rust: RustBinding::Path("Option<String>"),
                });
                args.push(Arg {
                    name: "instructions",
                    ty: HsType::maybe(HsType::Text),
                    rust: RustBinding::Path("Option<String>"),
                });
                args.push(Arg {
                    name: "lifetime",
                    ty: HsType::Named("WorkerLifetime"),
                    rust: RustBinding::Path("crate::WorkerLifetime"),
                });
                Verb {
                    ctor: "ForksStartWith",
                    method: "forks_start_with",
                    args,
                    ret: fallible(HsType::Tuple(vec![
                        HsType::Tuple(vec![HsType::Int, HsType::Int, HsType::Text]),
                        HsType::Named("WorktreeHandle"),
                    ])),
                    errors: None,
                    handling: HandlingClass::Actor,
                    }
            },
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
            Verb {
                ctor: "ForksPreviewWith",
                method: "forks_preview_with",
                args: vec![
                    Arg {
                        name: "role",
                        ty: HsType::Named("ActorLaunchRole"),
                        rust: RustBinding::External,
                    },
                    Arg {
                        name: "effectKeys",
                        ty: HsType::list(HsType::Named("ActorEffectKey")),
                        rust: RustBinding::Path("Vec<crate::ActorEffectKeyWire>"),
                    },
                    Arg {
                        name: "budget",
                        ty: HsType::maybe(HsType::Tuple(vec![HsType::Int, HsType::Int])),
                        rust: RustBinding::Path("Option<(i64, i64)>"),
                    },
                    Arg {
                        name: "model",
                        ty: HsType::maybe(HsType::Named("Model")),
                        rust: RustBinding::Path("Option<crate::Model>"),
                    },
                    Arg {
                        name: "effort",
                        ty: HsType::maybe(HsType::Named("ForkEffort")),
                        rust: RustBinding::Path("Option<crate::ForkEffort>"),
                    },
                    Arg {
                        name: "context",
                        ty: HsType::Named("ForkContext"),
                        rust: RustBinding::Path("crate::ForkContext"),
                    },
                    Arg {
                        name: "instructions",
                        ty: HsType::maybe(HsType::Text),
                        rust: RustBinding::Path("Option<String>"),
                    },
                    Arg {
                        name: "lifetime",
                        ty: HsType::Named("WorkerLifetime"),
                        rust: RustBinding::Path("crate::WorkerLifetime"),
                    },
                ],
                ret: fallible(HsType::Tuple(vec![
                    HsType::Tuple(vec![HsType::Text, HsType::Int, HsType::maybe(HsType::Int)]),
                    HsType::maybe(HsType::Named("WorkerLaunchPreview")),
                ])),
                errors: None,
                handling: HandlingClass::Actor,
            },
            group_verb("ForksCommitWith", "forks_commit_with"),
            group_verb("ForksCommitCapturedWith", "forks_commit_captured_with"),
            group_verb("ForksAbortWith", "forks_abort_with"),
            Verb {
                ctor: "ForksCleanupWith",
                method: "forks_cleanup_with",
                args: vec![Arg {
                    name: "forkGroup",
                    ty: HsType::Int,
                    rust: RustBinding::Derived,
                }],
                ret: HsType::Named("ForkGroupCleanupOutcome"),
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

fn group_verb(ctor: &'static str, method: &'static str) -> Verb {
    Verb {
        ctor,
        method,
        args: vec![Arg {
            name: "forkGroup",
            ty: HsType::Int,
            rust: RustBinding::Derived,
        }],
        ret: fallible(HsType::Unit),
        errors: None,
        handling: HandlingClass::Actor,
    }
}

fn fallible(ok: HsType) -> HsType {
    HsType::either(HsType::Text, ok)
}
