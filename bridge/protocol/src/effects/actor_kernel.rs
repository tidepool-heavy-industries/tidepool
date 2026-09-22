//! Kernel-private actor control boundary.
//!
//! The trusted `Tidepool.Actor` entry wrapper raises authored initialization
//! and behavior into a row containing this effect. Readiness and the two
//! phases of hidden mailbox settlement cross here. Authored actor rows and
//! model workbenches never contain `ActorKernel`.

use crate::schema::{
    Arg, Effect, HandlingClass, JsonInstance, Polymorphism, RustBinding, SumVariant, TypeDef,
    TypeShape, VariantFields, Verb, WireDerives,
};
use crate::HsType;

/// Runtime lifecycle facts a lifecycle source delivers. Live does not imply
/// application readiness; completion does not assert resource cleanup or
/// acceptance of delivered work. Haskell-only: the host builds these values
/// by constructor name when it answers `ActorLifecycleInputWith`.
fn actor_lifecycle() -> TypeDef {
    let text = |ctor: &'static str| SumVariant {
        ctor,
        fields: VariantFields::Positional(vec![HsType::Text]),
        doc: &[],
    };
    TypeDef {
        name: "ActorLifecycle",
        wire_rust: None,
        haskell_module: Some("Tidepool.Effects.Core"),
        shape: TypeShape::Sum {
            variants: vec![
                SumVariant {
                    ctor: "ActorLive",
                    fields: VariantFields::Positional(Vec::new()),
                    doc: &[],
                },
                text("ActorPaused"),
                text("ActorFinished"),
                text("ActorFailed"),
                text("ActorCancelled"),
            ],
        },
        json: JsonInstance::None,
        derives: WireDerives(&[]),
        domain: None,
        doc: &[
            "Runtime lifecycle facts. Live does not imply application readiness; ",
            "completion does not assert resource cleanup or acceptance of delivered work.",
        ],
    }
}

/// The private control effect used by the trusted actor wrappers.
#[must_use]
pub fn actor_kernel() -> Effect {
    Effect {
        name: "ActorKernel",
        authored_surface: crate::schema::AuthoredSurface::OPAQUE,
        handler: "ActorKernelDecodeHandler",
        handler_module: "actor_kernel",
        req_enum: "ActorKernelReq",
        decl_fn: "actor_kernel_decl",
        description: &[
            "Kernel-private actor control boundary. Trusted Tidepool.Actor wrappers use it for ",
            "readiness and hidden mailbox settlement; authored code has no operation here.",
        ],
        prompt_card: None,
        type_params: &[],
        default_row_args: &[],
        helpers_row_polymorphic: true,
        extra_imports: &["import Tidepool.Actor"],
        type_defs: vec![actor_lifecycle()],
        foreign_types: &[],
        errors: None,
        verbs: vec![
            Verb {
                ctor: "ActorInstallShutdownWith",
                method: "actor_install_shutdown_with",
                args: vec![
                    crate::schema::Arg {
                        name: "site",
                        ty: HsType::Int,
                        rust: crate::schema::RustBinding::Derived,
                    },
                    crate::schema::Arg {
                        name: "shutdown",
                        ty: HsType::func(
                            HsType::Int,
                            HsType::app(
                                HsType::app(HsType::Named("Eff"), HsType::Var("childEffs")),
                                HsType::Unit,
                            ),
                        ),
                        rust: crate::schema::RustBinding::HaskellValue,
                    },
                ],
                ret: HsType::Unit,
                errors: None,
                handling: HandlingClass::Actor,
                extract: None,
            },
            Verb {
                ctor: "ActorReadyWith",
                method: "actor_ready_with",
                args: Vec::new(),
                ret: HsType::Unit,
                errors: None,
                handling: HandlingClass::Actor,
                extract: None,
            },
            Verb {
                ctor: "ActorReplyWith",
                method: "actor_reply_with",
                args: vec![
                    crate::schema::Arg {
                        name: "site",
                        ty: HsType::Int,
                        rust: crate::schema::RustBinding::Derived,
                    },
                    crate::schema::Arg {
                        name: "reply",
                        ty: HsType::Var("result"),
                        rust: crate::schema::RustBinding::HaskellValue,
                    },
                ],
                ret: HsType::Unit,
                errors: None,
                handling: HandlingClass::Actor,
                extract: None,
            },
            Verb {
                ctor: "ActorContinueWith",
                method: "actor_continue_with",
                args: vec![
                    crate::schema::Arg {
                        name: "site",
                        ty: HsType::Int,
                        rust: crate::schema::RustBinding::Derived,
                    },
                    crate::schema::Arg {
                        name: "next",
                        ty: HsType::Var("next"),
                        rust: crate::schema::RustBinding::HaskellValue,
                    },
                ],
                ret: HsType::Unit,
                errors: None,
                handling: HandlingClass::Actor,
                extract: None,
            },
            source_install(
                "ActorInstallProgressSourceWith",
                "actor_install_progress_source_with",
                Arg {
                    name: "request",
                    ty: HsType::Int,
                    rust: RustBinding::Derived,
                },
            ),
            source_install(
                "ActorInstallSettlementSourceWith",
                "actor_install_settlement_source_with",
                Arg {
                    name: "request",
                    ty: HsType::Int,
                    rust: RustBinding::Derived,
                },
            ),
            source_install(
                "ActorInstallCommandSourceWith",
                "actor_install_command_source_with",
                Arg {
                    name: "job",
                    ty: HsType::Text,
                    rust: RustBinding::Path("String"),
                },
            ),
            source_install(
                "ActorInstallLifecycleSourceWith",
                "actor_install_lifecycle_source_with",
                Arg {
                    name: "target",
                    ty: HsType::Tuple(vec![HsType::Int, HsType::Int]),
                    rust: RustBinding::Path("(i64, i64)"),
                },
            ),
            // A source mapper asks for its delivered input with a request
            // whose reply type is CLOSED: the prepared route answers a
            // request from the host only against the reply type its
            // constructor declares (a bare type variable has no answer row),
            // so each source kind names its own input verb. Progress and
            // settlement sources reuse the `Replies` observation verbs, whose
            // reply types already live beside the request cells they read.
            Verb {
                ctor: "ActorLifecycleInputWith",
                method: "actor_lifecycle_input_with",
                args: vec![],
                ret: HsType::Named("ActorLifecycle"),
                errors: None,
                handling: HandlingClass::Actor,
                extract: None,
            },
            Verb {
                ctor: "ActorCommandInputWith",
                method: "actor_command_input_with",
                args: vec![],
                ret: HsType::Named("CommandResult"),
                errors: None,
                handling: HandlingClass::Actor,
                extract: None,
            },
        ],
        helpers: Vec::new(),
        polymorphism: Polymorphism::None,
        dispatched: false,
        caller_principal: false,
    }
}

fn source_install(ctor: &'static str, method: &'static str, target: Arg) -> Verb {
    Verb {
        ctor,
        method,
        args: vec![
            target,
            Arg {
                name: "entry",
                ty: HsType::func(
                    HsType::Int,
                    HsType::app(
                        HsType::app(HsType::Named("Eff"), HsType::Var("sourceEffs")),
                        HsType::Unit,
                    ),
                ),
                rust: RustBinding::HaskellValue,
            },
        ],
        ret: HsType::Unit,
        errors: None,
        handling: HandlingClass::Actor,
        extract: None,
    }
}
