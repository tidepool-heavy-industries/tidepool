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
            "Agent RPC with authored assignment and reply types: present a request to an attached model actor and resume the caller through its reply continuation. ",
            "Activation binds `sessionInput` and `respond` to those types. Define task-specific records or sums and compute with the returned value: map a report, match a decision, join independent answers or drive an actor transition. ",
            "The request remains an independent obligation: presentation, typed settlement and recipient incorporation are different evidence. ",
            "Use hosted lookup on `sessionInput` or `respond` when their exact current types are unknown. ",
            "The application uses its persistent workbench through the actor-local transport; the public request/reply facade owns authored operations.",
        ],
        prompt_card: Some(&[
            "Typed agent RPC: a request binds `sessionInput`, `sessionReply` and `respond` to your assignment and result types. Use local records and sums so replies compose with functions, joins and actor transitions; lookup exposes the current type. ",
            "A typed reply settles this request, while acceptance and incorporation belong to its recipient.",
        ]),
        type_params: &[],
        default_row_args: &[],
        extra_imports: &["import Tidepool.Agent.Session"],
        type_defs: Vec::new(),
        external_types: &[crate::schema::ExternalType {
            haskell_name: "RequestSite",
            rust_wire: "i64",
            core_module: Some("Tidepool.Internal.RequestSite"),
        }],
        errors: None,
        verbs: vec![
            Verb {
                ctor: "AgentSessionWith",
                method: "agent_session_with",
                args: vec![
                    Arg {
                        name: "site",
                        ty: HsType::app(
                            HsType::app(
                                HsType::Named("RequestSite"),
                                HsType::TypeCons(
                                    Box::new(HsType::Var("input")),
                                    Box::new(HsType::Var("extra")),
                                ),
                            ),
                            HsType::Var("output"),
                        ),
                        rust: RustBinding::External,
                    },
                    Arg {
                        name: "input",
                        ty: HsType::Var("input"),
                        rust: RustBinding::HaskellValue,
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
                    Arg {
                        // Other branches admitted in the same `unfold` as
                        // this request's target, if any: (label, allocated
                        // path, truncated preview) triples. Empty outside
                        // `Tidepool.Actors.Unfold.requestBranch`.
                        name: "siblings",
                        ty: HsType::list(HsType::Tuple(vec![
                            HsType::Text,
                            HsType::Text,
                            HsType::Text,
                        ])),
                        rust: RustBinding::Path("Vec<(String, String, String)>"),
                    },
                ],
                ret: HsType::Var("output"),
                errors: None,
                handling: HandlingClass::AgentSession,
            },
            response_publication("AgentSessionPublishResponseWith", "agent_session_publish_response_with", false),
            response_publication("AgentSessionPublishProgressResponseWith", "agent_session_publish_progress_response_with", true),
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
            },
        ],
        helpers: Vec::new(),
        polymorphism: Polymorphism::ResultBound { tyvar: "output" },
        generated_handler: false,
        handler_execution: crate::schema::HandlerExecution::Immediate,
        caller_principal: false,
    }
}

fn response_publication(ctor: &'static str, method: &'static str, progress: bool) -> Verb {
    let inputs = if progress {
        vec![
            HsType::Var("input"),
            HsType::Var("progress"),
            HsType::Var("response"),
        ]
    } else {
        vec![HsType::Var("input"), HsType::Var("response")]
    };
    Verb {
        ctor,
        method,
        args: vec![
            Arg {
                name: "requestId",
                ty: HsType::Int,
                rust: RustBinding::Derived,
            },
            Arg {
                name: "site",
                ty: HsType::app(
                    HsType::app(HsType::Named("RequestSite"), HsType::TypeList(inputs)),
                    HsType::Var("siteReply"),
                ),
                rust: RustBinding::External,
            },
            Arg {
                name: "response",
                ty: HsType::Var("response"),
                rust: RustBinding::HaskellValue,
            },
        ],
        ret: HsType::Unit,
        errors: None,
        handling: HandlingClass::AgentSession,
    }
}
