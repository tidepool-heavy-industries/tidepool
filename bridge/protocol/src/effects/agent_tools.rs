//! Private suspension boundary for a resident Haskell tool policy.
//!
//! `Tidepool.Agent.Contract.serveTools` is the authored surface. The policy
//! compiles one tools record, publishes its declarations while awaiting an
//! invocation, dispatches the invocation in Haskell, publishes the result,
//! and repeats. Rust owns host projection and actor admission; no live closure
//! crosses the language boundary.

use crate::hs::HsType;
use crate::schema::{Arg, Effect, HandlingClass, Polymorphism, RustBinding, Verb};

#[must_use]
pub fn agent_tools() -> Effect {
    Effect {
        name: "AgentTools",
        authored_surface: crate::schema::AuthoredSurface::OPAQUE,
        handler: "AgentToolsDecodeHandler",
        handler_module: "agent_tools",
        req_enum: "AgentToolsReq",
        decl_fn: "agent_tools_decl",
        description: &[
            "Private resident-policy boundary for tools exposed to an attached agent. Authored ",
            "code uses `serveTools`; Rust owns host projection while Haskell owns declarations ",
            "and dispatch.",
        ],
        prompt_card: None,
        type_params: &[],
        default_row_args: &[],
        extra_imports: &[],
        type_defs: Vec::new(),
        external_types: &[crate::schema::ExternalType {
            haskell_name: "RequestSite",
            rust_wire: "i64",
            core_module: Some("Tidepool.Internal.RequestSite"),
        }],
        errors: None,
        verbs: vec![
            Verb {
                ctor: "AgentToolsInstallWith",
                method: "agent_tools_install_with",
                args: vec![
                    Arg {
                        name: "declarations",
                        ty: HsType::Value,
                        rust: RustBinding::HaskellValue,
                    },
                    Arg {
                        name: "dispatch",
                        ty: HsType::func(
                            HsType::Int,
                            HsType::app(
                                HsType::app(HsType::Named("Eff"), HsType::Var("toolEffs")),
                                HsType::Text,
                            ),
                        ),
                        rust: RustBinding::HaskellValue,
                    },
                ],
                ret: HsType::Unit,
                errors: None,
                handling: HandlingClass::Actor,
            },
            Verb {
                ctor: "AgentToolsInstallReceiverWith",
                method: "agent_tools_install_receiver_with",
                args: vec![
                    Arg {
                        name: "site",
                        ty: HsType::app(
                            HsType::app(HsType::Named("RequestSite"), HsType::TypeList(vec![])),
                            HsType::Unit,
                        ),
                        rust: RustBinding::External,
                    },
                    Arg {
                        name: "receiver",
                        ty: HsType::func(
                            HsType::Int,
                            HsType::app(
                                HsType::app(HsType::Named("Eff"), HsType::Var("receiverEffs")),
                                HsType::Unit,
                            ),
                        ),
                        rust: RustBinding::HaskellValue,
                    },
                ],
                ret: HsType::Unit,
                errors: None,
                handling: HandlingClass::Actor,
            },
            Verb {
                ctor: "AgentToolsInputWith",
                method: "agent_tools_input_with",
                args: vec![],
                ret: HsType::Tuple(vec![HsType::Text, HsType::Value]),
                errors: None,
                handling: HandlingClass::Actor,
            },
            Verb {
                ctor: "AgentToolsAwaitWith",
                method: "agent_tools_await_with",
                args: vec![
                    Arg {
                        name: "declarations",
                        ty: HsType::Value,
                        rust: RustBinding::HaskellValue,
                    },
                    Arg {
                        name: "synopsis",
                        ty: HsType::Text,
                        rust: RustBinding::Derived,
                    },
                    Arg {
                        name: "initialUserMessage",
                        ty: HsType::maybe(HsType::Text),
                        rust: RustBinding::Derived,
                    },
                ],
                ret: HsType::Tuple(vec![HsType::Text, HsType::Value]),
                errors: None,
                handling: HandlingClass::Actor,
            },
            Verb {
                ctor: "AgentToolsReplyWith",
                method: "agent_tools_reply_with",
                args: vec![Arg {
                    name: "result",
                    ty: HsType::Value,
                    rust: RustBinding::HaskellValue,
                }],
                ret: HsType::Unit,
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
