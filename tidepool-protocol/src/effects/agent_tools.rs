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
        helpers_row_polymorphic: true,
        extra_imports: &[],
        type_defs: Vec::new(),
        foreign_types: &[],
        errors: None,
        verbs: vec![
            Verb {
                ctor: "AgentToolsInstallWith",
                method: "agent_tools_install_with",
                args: vec![
                    Arg {
                        name: "declarations",
                        ty: HsType::Value,
                        rust: RustBinding::CoreValue,
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
                        rust: RustBinding::CoreValue,
                    },
                ],
                ret: HsType::Unit,
                errors: None,
                handling: HandlingClass::Actor,
                extract: None,
            },
            Verb {
                ctor: "AgentToolsInputWith",
                method: "agent_tools_input_with",
                args: vec![],
                ret: HsType::Tuple(vec![HsType::Text, HsType::Value]),
                errors: None,
                handling: HandlingClass::Actor,
                extract: None,
            },
            Verb {
                ctor: "AgentToolsAwaitWith",
                method: "agent_tools_await_with",
                args: vec![
                    Arg {
                        name: "declarations",
                        ty: HsType::Value,
                        rust: RustBinding::CoreValue,
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
                extract: None,
            },
            Verb {
                ctor: "AgentToolsReplyWith",
                method: "agent_tools_reply_with",
                args: vec![Arg {
                    name: "result",
                    ty: HsType::Value,
                    rust: RustBinding::CoreValue,
                }],
                ret: HsType::Unit,
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
