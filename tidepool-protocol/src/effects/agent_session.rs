//! Typed boundary for a supervised interactive-agent session.
//!
//! Rust does not call a provider itself. It publishes one actor-local
//! persistent-Haskell transport. Request settlement is a separate reply
//! suspension carrying the statically checked result.

use crate::hs::HsType;
use crate::schema::{Arg, Effect, HandlingClass, Polymorphism, RustBinding, Verb};

#[must_use]
pub fn agent_session() -> Effect {
    Effect {
        name: "AgentSession",
        authored_surface: crate::schema::AuthoredSurface::OPAQUE,
        handler: "AgentSessionDecodeHandler",
        handler_module: "agent_session",
        req_enum: "AgentSessionReq",
        decl_fn: "agent_session_decl",
        description: &[
            "Present one typed request to this actor's attached application. The external ",
            "application reaches the persistent Haskell workbench through its actor-local ",
            "transport.",
        ],
        prompt_card: Some(&[
            "A request activation mounts typed `sessionInput`, `sessionReply`, and `respond` in ",
            "the attached application.",
        ]),
        type_params: &[],
        default_row_args: &[],
        helpers_row_polymorphic: true,
        extra_imports: &["import Tidepool.Agent.Session"],
        type_defs: Vec::new(),
        foreign_types: &[],
        errors: None,
        verbs: vec![
            Verb {
                ctor: "AgentSessionWith",
                method: "agent_session_with",
                args: vec![
                    Arg {
                        name: "site",
                        ty: HsType::Int,
                        rust: RustBinding::Derived,
                    },
                    Arg {
                        name: "input",
                        ty: HsType::Var("input"),
                        rust: RustBinding::CoreValue,
                    },
                    Arg {
                        name: "requestId",
                        ty: HsType::Int,
                        rust: RustBinding::Derived,
                    },
                    Arg {
                        name: "initialUserMessage",
                        ty: HsType::maybe(HsType::Text),
                        rust: RustBinding::Derived,
                    },
                ],
                ret: HsType::Var("output"),
                errors: None,
                handling: HandlingClass::AgentSession,
                extract: None,
            },
            Verb {
                ctor: "AgentAttachWith",
                method: "agent_attach_with",
                args: vec![Arg {
                    name: "initialUserMessage",
                    ty: HsType::maybe(HsType::Text),
                    rust: RustBinding::Derived,
                }],
                ret: HsType::Unit,
                errors: None,
                handling: HandlingClass::AgentSession,
                extract: None,
            },
        ],
        helpers: Vec::new(),
        polymorphism: Polymorphism::ResultBound { tyvar: "output" },
        dispatched: false,
    }
}
