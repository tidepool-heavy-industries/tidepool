//! The actor-runtime suspension boundary.
//!
//! The public Haskell API lives in `Tidepool.Actor`. Starts and replacements
//! carry compiler-issued exit witnesses and a row-typed child entry. Typed
//! terminal observations borrow or import the runtime-owned exit snapshot.

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

fn exit_type() -> HsType {
    HsType::app(HsType::Named("ActorExit"), HsType::Var("exit"))
}

fn exit_site(reply: HsType) -> Arg {
    Arg {
        name: "site",
        ty: HsType::app(
            HsType::app(
                HsType::Named("RequestSite"),
                HsType::TypeList(vec![HsType::Var("exit")]),
            ),
            reply,
        ),
        rust: RustBinding::External,
    }
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
            "Author small control languages and the stateful machines that interpret them with `Tidepool.Actor` and record actors. ",
            "Calls and event payloads form the vocabulary; private state retains the work; handlers compose transitions, replies, agent requests and Jev-selected continuations. ",
            "Compose endpoint values to connect machines. Map sources into a shared event sum with `fmap` and merge them with `<>`; products hold jointly needed facts. ",
            "A mailbox serializes one actor's handlers, including suspended ones; independent actors can progress separately. Use a later event to continue work that requires another handler on the same mailbox. ",
            "`awaitExit` observes `Completed value`, `Failed reason`, or `Cancelled reason` repeatedly, including closure-valued exits. ",
            "Request settlement, actor exit and caller acceptance are different events. Use response/watch APIs for task results and typed stop/cleanup for lifecycle. ",
            "The constructors in this effect are runtime substrate; author against the public facade.",
        ],
        prompt_card: Some(&[
            "`Tidepool.Actor`: author a control language with typed calls and events, then interpret it through stateful handlers. Compose endpoint values, event sums, agent requests and Jev continuations into orchestration machines. ",
            "`awaitExit ref` returns `Completed value`, `Failed reason`, or `Cancelled reason` repeatably, preserving closure-valued exits. Task reply, actor exit and acceptance remain distinct.",
        ]),
        type_params: &[],
        default_row_args: &[],
        extra_imports: &["import Tidepool.Actor"],
        type_defs: vec![

            TypeDef {
                name: "ActorEffectProfile",
                wire_rust: None,
                haskell_module: None,
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
                        SumVariant {
                            ctor: "ActorSelectedProfile",
                            fields: positional_fields![HsType::List(Box::new(HsType::Named(
                                "ActorEffectKey"
                            )))],
                            doc: &[
                                "Explicit effect row; resource authority remains runtime-owned.",
                            ],
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
                haskell_module: None,
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
                haskell_module: None,
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
        external_types: &[crate::schema::ExternalType {
            haskell_name: "RequestSite", rust_wire: "i64", core_module: Some("Tidepool.Internal.RequestSite"),
        }, crate::schema::ExternalType {
            haskell_name: "ActorRef", rust_wire: "(i64, i64)", core_module: Some("Tidepool.Internal.ActorRef"),
        }, crate::schema::ExternalType {
            haskell_name: "ActorExit", rust_wire: "()", core_module: Some("Tidepool.Internal.ActorExit"),
        }, crate::schema::ExternalType {
            haskell_name: "WorkspaceHandle",
            rust_wire: "tidepool_bridge_effects::WtWorkspaceHandle",
            core_module: None,
        }, crate::schema::ExternalType {
            haskell_name: "ActorEffectKey",
            rust_wire: "crate::ActorEffectKeyWire",
            core_module: None,
        }],
        errors: None,
        verbs: vec![

            Verb {
                ctor: "ActorStartWith",
                method: "actor_start_with",
                args: vec![
                    Arg {
                        name: "label",
                        ty: HsType::Text,
                        rust: RustBinding::Derived,
                    },
                    exit_site(HsType::Var("siteReply")),
                    Arg {
                        name: "entry",
                        ty: HsType::func(
                            HsType::Int,
                            HsType::app(
                                HsType::app(HsType::Named("Eff"), HsType::Var("childEffs")),
                                HsType::Unit,
                            ),
                        ),
                        rust: RustBinding::HaskellValue,
                    },
                    Arg {
                        name: "profile",
                        ty: HsType::Named("ActorEffectProfile"),
                        rust: RustBinding::Path("crate::ActorEffectProfileWire"),
                    },
                    Arg {
                        name: "workspace",
                        ty: HsType::Maybe(Box::new(HsType::Named("WorkspaceHandle"))),
                        rust: RustBinding::External,
                    },
                ],
                ret: launched_actor_type(),
                errors: None,
                handling: HandlingClass::Actor,
            },



            Verb {
                ctor: "ActorWaitWith",
                method: "actor_wait_with",
                args: vec![exit_site(exit_type()), Arg {
                    name: "actor",
                    ty: address_type(),
                    rust: RustBinding::Path("(i64, i64)"),
                }],
                ret: exit_type(),
                errors: None,
                handling: HandlingClass::Actor,
            },
            Verb {
                ctor: "ActorPollWith",
                method: "actor_poll_with",
                args: vec![exit_site(HsType::maybe(exit_type())), Arg {
                    name: "actor",
                    ty: address_type(),
                    rust: RustBinding::Path("(i64, i64)"),
                }],
                ret: HsType::maybe(exit_type()),
                errors: None,
                handling: HandlingClass::Actor,
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
                        rust: RustBinding::HaskellValue,
                    },
                ],
                ret: HsType::Var("result"),
                errors: None,
                handling: HandlingClass::Actor,
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
                        rust: RustBinding::HaskellValue,
                    },
                ],
                ret: HsType::Named("ActorCallStatus"),
                errors: None,
                handling: HandlingClass::Actor,
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
                        rust: RustBinding::HaskellValue,
                    },
                ],
                ret: HsType::Unit,
                errors: None,
                handling: HandlingClass::Actor,
            },
            Verb {
                ctor: "ActorTryCastWith",
                method: "actor_try_cast_with",
                args: vec![
                    Arg {
                        name: "actor",
                        ty: address_type(),
                        rust: RustBinding::Path("(i64, i64)"),
                    },
                    Arg {
                        name: "request",
                        ty: HsType::app(HsType::Var("protocol"), HsType::Unit),
                        rust: RustBinding::HaskellValue,
                    },
                ],
                ret: HsType::either(HsType::Text, HsType::Unit),
                errors: None,
                handling: HandlingClass::Actor,
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
            },
            Verb {
                ctor: "ActorReplaceWith",
                method: "actor_replace_with",
                args: vec![
                    Arg {
                        name: "actor",
                        ty: address_type(),
                        rust: RustBinding::Path("(i64, i64)"),
                    },
                    exit_site(HsType::Var("siteReply")),
                    Arg {
                        name: "entry",
                        ty: HsType::func(
                            HsType::Var("exit"),
                            HsType::app(
                                HsType::app(HsType::Named("Eff"), HsType::Var("childEffs")),
                                HsType::Unit,
                            ),
                        ),
                        rust: RustBinding::HaskellValue,
                    },
                    Arg {
                        name: "label",
                        ty: HsType::Text,
                        rust: RustBinding::Derived,
                    },
                    Arg {
                        name: "profile",
                        ty: HsType::Named("ActorEffectProfile"),
                        rust: RustBinding::Path("crate::ActorEffectProfileWire"),
                    },
                ],
                ret: address_type(),
                errors: None,
                handling: HandlingClass::Actor,
            },
        ],
        // The public wrappers thread protected sites from their authored calls.
        helpers: Vec::new(),
        polymorphism: Polymorphism::None,
        generated_handler: false,
        handler_execution: crate::schema::HandlerExecution::Immediate,
        caller_principal: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actor_entries_remain_the_field_two_live_payload() {
        let effect = actor();
        for constructor in ["ActorStartWith", "ActorReplaceWith"] {
            let verb = effect
                .verbs
                .iter()
                .find(|verb| verb.ctor == constructor)
                .unwrap();
            assert!(matches!(verb.args[2].rust, RustBinding::HaskellValue));
            assert_eq!(
                verb.args
                    .iter()
                    .filter(|argument| matches!(argument.rust, RustBinding::HaskellValue))
                    .count(),
                1
            );
        }
    }
}
