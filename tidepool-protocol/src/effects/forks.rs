//! Cache-preserving actor-fork admission capability.

use crate::hs::HsType;
use crate::schema::{
    Arg, Effect, HandlingClass, JsonInstance, Polymorphism, RustBinding, SumVariant, TypeDef,
    TypeShape, VariantFields, Verb, WireDerives,
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
        description: &["Private atomic context-fork admission substrate used by `unfold`."],
        prompt_card: None,
        type_params: &[],
        default_row_args: &[],
        helpers_row_polymorphic: true,
        extra_imports: &[],
        type_defs: vec![
            TypeDef {
                name: "ForkEffort",
                wire_rust: None,
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
        foreign_types: &[
            ("ActorLaunchRole", "crate::ActorLaunchRoleWire"),
            ("ActorEffectProfile", "crate::ActorEffectProfileWire"),
            ("WorktreeSpec", "tidepool_bridge_effects::WtWorktreeSpec"),
            ("DirtyPolicy", "tidepool_bridge_effects::WtDirtyPolicy"),
            (
                "WorktreeHandle",
                "tidepool_bridge_effects::WtWorktreeHandle",
            ),
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
                extract: None,
            },
            {
                let mut args = launch_args(true);
                args.push(Arg {
                    name: "worktreeSpec",
                    ty: HsType::maybe(HsType::Named("WorktreeSpec")),
                    rust: RustBinding::Path("Option<tidepool_bridge_effects::WtWorktreeSpec>"),
                });
                args.push(Arg {
                    name: "boundDirtyPolicy",
                    ty: HsType::Named("DirtyPolicy"),
                    rust: RustBinding::Path("tidepool_bridge_effects::WtDirtyPolicy"),
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
                    extract: None,
                }
            },
            group_verb("ForksCommitWith", "forks_commit_with"),
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
                extract: None,
            },
        ],
        helpers: Vec::new(),
        polymorphism: Polymorphism::None,
        dispatched: false,
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
        extract: None,
    }
}

fn fallible(ok: HsType) -> HsType {
    HsType::either(HsType::Text, ok)
}
