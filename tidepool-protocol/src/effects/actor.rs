//! The actor-runtime suspension boundary.
//!
//! The public Haskell API lives in `Tidepool.Actor`; this schema owns only the
//! outward runtime requests. A start carries one
//! existentially row-typed child entry as its field-1 live payload. A wait
//! carries an exact Rust routing identity and returns terminal metadata. The
//! successful exit value never crosses either request: it remains in the
//! managed Haskell cell carried by the corresponding `ActorRef`.
//!
//! There is no `tidepool-handlers` handler.  The request is decoded and
//! serviced by `tidepool-actor`, whose registry owns exact-incarnation wait
//! semantics.

use crate::hs::HsType;
use crate::schema::{
    Arg, Effect, HandlingClass, JsonInstance, Polymorphism, RustBinding, SumVariant, TypeDef,
    TypeShape, Verb, WireDerive, WireDerives,
};
fn address_type() -> HsType {
    HsType::Tuple(vec![HsType::Int, HsType::Int])
}

fn launched_actor_type() -> HsType {
    HsType::Tuple(vec![HsType::Int, HsType::Int, HsType::Text])
}

/// The actor effect's internal start and wait requests.
#[must_use]
pub fn actor() -> Effect {
    Effect {
        name: "Actor",
        authored_surface: crate::schema::AuthoredSurface::OPAQUE,
        handler: "ActorDecodeHandler",
        handler_module: "actor",
        req_enum: "ActorReq",
        decl_fn: "actor_decl",
        description: &[
            "Typed actor lifecycle and communication. Authored code uses `Tidepool.Actor`; ",
            "the constructors in this effect are runtime substrate, not a second public API.",
        ],
        prompt_card: Some(&[
            "`Tidepool.Actor` provides exact-incarnation actor references and typed exit ",
            "observation. `awaitExit ref` returns `Completed value`, `Failed reason`, or ",
            "`Cancelled reason`; it is repeatable and preserves closure-valued exits.",
        ]),
        type_params: &[],
        default_row_args: &[],
        helpers_row_polymorphic: true,
        extra_imports: &["import Tidepool.Actor"],
        type_defs: vec![
            TypeDef {
                name: "ActorLaunchRole",
                wire_rust: None,
                shape: TypeShape::Sum {
                    variants: vec![
                        SumVariant {
                            ctor: "ActorRootRole",
                            fields: positional_fields![],
                            doc: &[],
                        },
                        SumVariant {
                            ctor: "ActorResearchRole",
                            fields: positional_fields![],
                            doc: &[],
                        },
                        SumVariant {
                            ctor: "ActorCodingRole",
                            fields: positional_fields![],
                            doc: &[],
                        },
                        SumVariant {
                            ctor: "ActorScaffoldingRole",
                            fields: positional_fields![],
                            doc: &[],
                        },
                        SumVariant {
                            ctor: "ActorIntegrationRole",
                            fields: positional_fields![],
                            doc: &[],
                        },
                        SumVariant {
                            ctor: "ActorInheritedRole",
                            fields: positional_fields![],
                            doc: &[],
                        },
                    ],
                },
                json: JsonInstance::None,
                derives: WireDerives(&[
                    WireDerive::Debug,
                    WireDerive::Clone,
                    WireDerive::PartialEq,
                    WireDerive::Eq,
                ]),
                domain: None,
                doc: &["Semantic role selected before runtime policy projection."],
            },
            TypeDef {
                name: "ActorEffectProfile",
                wire_rust: None,
                shape: TypeShape::Sum {
                    variants: vec![
                        SumVariant {
                            ctor: "ActorReadWriteProfile",
                            fields: positional_fields![],
                            doc: &[],
                        },
                        SumVariant {
                            ctor: "ActorReadOnlyProfile",
                            fields: positional_fields![],
                            doc: &[],
                        },
                    ],
                },
                json: JsonInstance::None,
                derives: WireDerives(&[
                    WireDerive::Debug,
                    WireDerive::Clone,
                    WireDerive::PartialEq,
                    WireDerive::Eq,
                ]),
                domain: None,
                doc: &["Closed interpreter profile selected for one actor incarnation."],
            },
            TypeDef {
                name: "ActorTerminalStatus",
                wire_rust: None,
                shape: TypeShape::Sum {
                    variants: vec![
                        SumVariant {
                            ctor: "ActorCompletedStatus",
                            fields: positional_fields![],
                            doc: &[],
                        },
                        SumVariant {
                            ctor: "ActorFailedStatus",
                            fields: positional_fields![HsType::Text],
                            doc: &[],
                        },
                        SumVariant {
                            ctor: "ActorCancelledStatus",
                            fields: positional_fields![HsType::Text],
                            doc: &[],
                        },
                    ],
                },
                json: JsonInstance::None,
                derives: WireDerives(&[
                    WireDerive::Debug,
                    WireDerive::Clone,
                    WireDerive::PartialEq,
                    WireDerive::Eq,
                ]),
                domain: None,
                doc: &[],
            },
            TypeDef {
                name: "ActorCallStatus",
                wire_rust: None,
                shape: TypeShape::Sum {
                    variants: vec![
                        SumVariant {
                            ctor: "ActorCallSucceeded",
                            fields: positional_fields![],
                            doc: &[],
                        },
                        SumVariant {
                            ctor: "ActorCallFailed",
                            fields: positional_fields![HsType::Text],
                            doc: &[],
                        },
                    ],
                },
                json: JsonInstance::None,
                derives: WireDerives(&[
                    WireDerive::Debug,
                    WireDerive::Clone,
                    WireDerive::PartialEq,
                    WireDerive::Eq,
                ]),
                domain: None,
                doc: &["Result of a unit-returning actor call whose lifecycle failure is data."],
            },
        ],
        foreign_types: &[],
        errors: None,
        verbs: vec![
            Verb {
                ctor: "ActorBeginForkGroupWith",
                method: "actor_begin_fork_group_with",
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
                ret: HsType::Tuple(vec![
                    HsType::Int,
                    HsType::Text,
                    HsType::List(Box::new(HsType::Text)),
                ]),
                errors: None,
                handling: HandlingClass::Actor,
                extract: None,
            },
            Verb {
                ctor: "ActorStartWith",
                method: "actor_start_with",
                args: vec![
                    Arg {
                        name: "label",
                        ty: HsType::Text,
                        rust: RustBinding::Derived,
                    },
                    Arg {
                        name: "entry",
                        ty: HsType::func(
                            HsType::Int,
                            HsType::app(
                                HsType::app(HsType::Named("Eff"), HsType::Var("childEffs")),
                                HsType::Unit,
                            ),
                        ),
                        rust: RustBinding::CoreValue,
                    },
                    Arg {
                        name: "role",
                        ty: HsType::Named("ActorLaunchRole"),
                        rust: RustBinding::Path("crate::ActorLaunchRoleWire"),
                    },
                    Arg {
                        name: "profile",
                        ty: HsType::Named("ActorEffectProfile"),
                        rust: RustBinding::Path("crate::ActorEffectProfileWire"),
                    },
                    Arg {
                        name: "launchWorktrees",
                        ty: HsType::List(Box::new(HsType::Text)),
                        rust: RustBinding::Derived,
                    },
                ],
                ret: launched_actor_type(),
                errors: None,
                handling: HandlingClass::Actor,
                extract: None,
            },
            Verb {
                ctor: "ActorForkWith",
                method: "actor_fork_with",
                args: vec![
                    Arg {
                        name: "label",
                        ty: HsType::Text,
                        rust: RustBinding::Derived,
                    },
                    Arg {
                        name: "entry",
                        ty: HsType::func(
                            HsType::Int,
                            HsType::app(
                                HsType::app(HsType::Named("Eff"), HsType::Var("childEffs")),
                                HsType::Unit,
                            ),
                        ),
                        rust: RustBinding::CoreValue,
                    },
                    Arg {
                        name: "forkGroup",
                        ty: HsType::Int,
                        rust: RustBinding::Derived,
                    },
                    Arg {
                        name: "role",
                        ty: HsType::Named("ActorLaunchRole"),
                        rust: RustBinding::Path("crate::ActorLaunchRoleWire"),
                    },
                    Arg {
                        name: "profile",
                        ty: HsType::Named("ActorEffectProfile"),
                        rust: RustBinding::Path("crate::ActorEffectProfileWire"),
                    },
                    Arg {
                        name: "launchWorktrees",
                        ty: HsType::List(Box::new(HsType::Text)),
                        rust: RustBinding::Derived,
                    },
                ],
                ret: launched_actor_type(),
                errors: None,
                handling: HandlingClass::Actor,
                extract: None,
            },
            Verb {
                ctor: "ActorCommitForkGroupWith",
                method: "actor_commit_fork_group_with",
                args: vec![Arg {
                    name: "forkGroup",
                    ty: HsType::Int,
                    rust: RustBinding::Derived,
                }],
                ret: HsType::Unit,
                errors: None,
                handling: HandlingClass::Actor,
                extract: None,
            },
            Verb {
                ctor: "ActorAbortForkGroupWith",
                method: "actor_abort_fork_group_with",
                args: vec![Arg {
                    name: "forkGroup",
                    ty: HsType::Int,
                    rust: RustBinding::Derived,
                }],
                ret: HsType::Unit,
                errors: None,
                handling: HandlingClass::Actor,
                extract: None,
            },
            Verb {
                ctor: "ActorWaitWith",
                method: "actor_wait_with",
                args: vec![Arg {
                    name: "actor",
                    ty: address_type(),
                    rust: RustBinding::Path("(i64, i64)"),
                }],
                ret: HsType::Named("ActorTerminalStatus"),
                errors: None,
                handling: HandlingClass::Actor,
                extract: None,
            },
            Verb {
                ctor: "ActorPollWith",
                method: "actor_poll_with",
                args: vec![Arg {
                    name: "actor",
                    ty: address_type(),
                    rust: RustBinding::Path("(i64, i64)"),
                }],
                ret: HsType::maybe(HsType::Named("ActorTerminalStatus")),
                errors: None,
                handling: HandlingClass::Actor,
                extract: None,
            },
            Verb {
                ctor: "ActorCallWith",
                method: "actor_call_with",
                args: vec![
                    Arg {
                        name: "actor",
                        ty: address_type(),
                        rust: RustBinding::Path("(i64, i64)"),
                    },
                    Arg {
                        name: "request",
                        ty: HsType::app(HsType::Var("protocol"), HsType::Var("result")),
                        rust: RustBinding::CoreValue,
                    },
                ],
                ret: HsType::Var("result"),
                errors: None,
                handling: HandlingClass::Actor,
                extract: None,
            },
            Verb {
                ctor: "ActorTryCallWith",
                method: "actor_try_call_with",
                args: vec![
                    Arg {
                        name: "actor",
                        ty: address_type(),
                        rust: RustBinding::Path("(i64, i64)"),
                    },
                    Arg {
                        name: "request",
                        ty: HsType::app(HsType::Var("protocol"), HsType::Unit),
                        rust: RustBinding::CoreValue,
                    },
                ],
                ret: HsType::Named("ActorCallStatus"),
                errors: None,
                handling: HandlingClass::Actor,
                extract: None,
            },
            Verb {
                ctor: "ActorCastWith",
                method: "actor_cast_with",
                args: vec![
                    Arg {
                        name: "actor",
                        ty: address_type(),
                        rust: RustBinding::Path("(i64, i64)"),
                    },
                    Arg {
                        name: "request",
                        ty: HsType::app(HsType::Var("protocol"), HsType::Unit),
                        rust: RustBinding::CoreValue,
                    },
                ],
                ret: HsType::Unit,
                errors: None,
                handling: HandlingClass::Actor,
                extract: None,
            },
            Verb {
                ctor: "ActorDrainWith",
                method: "actor_drain_with",
                args: vec![Arg {
                    name: "actor",
                    ty: address_type(),
                    rust: RustBinding::Path("(i64, i64)"),
                }],
                ret: HsType::Unit,
                errors: None,
                handling: HandlingClass::Actor,
                extract: None,
            },
        ],
        // The public wrapper needs the managed cell carried by `ActorRef`, so
        // it is authored in Tidepool.Actor rather than emitted as a second,
        // raw helper here.
        helpers: Vec::new(),
        polymorphism: Polymorphism::None,
        dispatched: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actor_entries_remain_the_field_one_live_payload() {
        let effect = actor();
        for constructor in ["ActorStartWith", "ActorForkWith"] {
            let verb = effect
                .verbs
                .iter()
                .find(|verb| verb.ctor == constructor)
                .unwrap();
            assert!(matches!(verb.args[1].rust, RustBinding::CoreValue));
            assert_eq!(
                verb.args
                    .iter()
                    .filter(|argument| matches!(argument.rust, RustBinding::CoreValue))
                    .count(),
                1
            );
        }
    }
}
