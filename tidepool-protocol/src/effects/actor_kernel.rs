//! Kernel-private actor control boundary.
//!
//! The trusted `Tidepool.Actor` entry wrapper raises authored initialization
//! and behavior into a row containing this effect. Readiness and the two
//! phases of hidden mailbox settlement cross here. Authored actor rows and
//! model workbenches never contain `ActorKernel`.

use crate::schema::{Effect, HandlingClass, Polymorphism, Verb};
use crate::HsType;

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
        type_defs: Vec::new(),
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
                        rust: crate::schema::RustBinding::CoreValue,
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
                        rust: crate::schema::RustBinding::CoreValue,
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
                        rust: crate::schema::RustBinding::CoreValue,
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
            ),
            source_install(
                "ActorInstallSettlementSourceWith",
                "actor_install_settlement_source_with",
            ),
            Verb {
                ctor: "ActorSourceInputWith",
                method: "actor_source_input_with",
                args: vec![],
                ret: HsType::Var("event"),
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

fn source_install(ctor: &'static str, method: &'static str) -> Verb {
    use crate::schema::{Arg, RustBinding};
    Verb {
        ctor,
        method,
        args: vec![
            Arg {
                name: "request",
                ty: HsType::Int,
                rust: RustBinding::Derived,
            },
            Arg {
                name: "entry",
                ty: HsType::func(
                    HsType::Int,
                    HsType::app(
                        HsType::app(HsType::Named("Eff"), HsType::Var("sourceEffs")),
                        HsType::Unit,
                    ),
                ),
                rust: RustBinding::CoreValue,
            },
        ],
        ret: HsType::Unit,
        errors: None,
        handling: HandlingClass::Actor,
        extract: None,
    }
}
