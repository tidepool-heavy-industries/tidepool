//! Private suspension boundary for a resident Haskell MCP policy.
//!
//! `Tidepool.Agent.Contract.serveTools` is the authored surface. The policy
//! compiles one tools record, publishes its declarations while awaiting an
//! invocation, dispatches the invocation in Haskell, publishes the result,
//! and repeats. Rust owns MCP transport and actor admission; no live closure
//! crosses the language boundary.

use crate::hs::HsType;
use crate::schema::{Arg, Effect, HandlingClass, Polymorphism, RustBinding, Verb};

#[must_use]
pub fn actor_mcp() -> Effect {
    Effect {
        name: "ActorMcp",
        authored_surface: crate::schema::AuthoredSurface::OPAQUE,
        handler: "ActorMcpDecodeHandler",
        handler_module: "actor_mcp",
        req_enum: "ActorMcpReq",
        decl_fn: "actor_mcp_decl",
        description: &[
            "Private resident-policy boundary for actor-scoped MCP tools. Authored code uses ",
            "`serveTools`; Rust owns transport while Haskell owns declarations and dispatch.",
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
                ctor: "ActorMcpAwaitWith",
                method: "actor_mcp_await_with",
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
                ctor: "ActorMcpReplyWith",
                method: "actor_mcp_reply_with",
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
