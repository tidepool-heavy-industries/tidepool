//! The `Fork` suspension.
//!
//! `Tidepool.Fork`'s `fork @T brief` / `forkAll @T briefs` — the SEND-and-join
//! child-agent-session fork, spawn-and-gather rather than `RunLLMTurn`'s
//! suspend-and-resume. Polymorphic response type, bound at the invocation
//! site: `fork`/`forkAll`/`forkMap`/`forkCata` (`Tidepool.Fork`, authored
//! library code, not generated here) are OPAQUE stubs the extractor head-swaps
//! to `forkSited`/`forkAllSited` — this effect's own two helpers — substituting
//! a fresh per-call-site literal `Int` for the `0` placeholder in their bodies
//! (`Translate.hs`, matched by name). Both wrap a concrete `Value` result and
//! `unsafeCoerce` it back to the caller's answer type, safe because the
//! extractor has already checked the call site's answer type is monomorphic
//! (`checkRunLLMTurnType`) — see `run_llm_turn.rs`'s module doc for the shared
//! reasoning.
//!
//! No real `tidepool-handlers` handler (harness-serviced only, same
//! convention as `AskUser`/`ReadState`) — only `decl_rs`/`suspension_req_rs`
//! consume this definition.

use crate::hs::HsType;
use crate::schema::{
    Arg, Effect, HandlingClass, Helper, HelperBody, Polymorphism, RustBinding, SitedCtorArg, Verb,
};

fn site_arg() -> Arg {
    Arg {
        name: "site",
        ty: HsType::Int,
        rust: RustBinding::Derived,
    }
}

/// The `Fork` suspension, decode-only.
#[must_use]
pub fn fork() -> Effect {
    Effect {
        name: "Fork",
        authored_surface: crate::schema::AuthoredSurface::All,
        handler: "ForkHandler",
        handler_module: "fork",
        req_enum: "ForkReq",
        decl_fn: "fork_decl",
        description: &[
            "Spawn parallel sub-answerers and gather their typed answers. ",
            "`fork \\@T brief` forks ONE child that answers a single `T`; ",
            "`forkAll \\@T briefs` forks one child per brief, answered together ",
            "as `[T]` in order (`import Tidepool.Fork`). A forked child answers ",
            "its own brief and may fork further, within driver-enforced depth ",
            "and descendant budgets.",
        ],
        prompt_card: Some(&[
            "`fork @T brief :: M T` — delegate to one sub-answerer that answers `brief` ",
            "on its own.\n",
            "`forkAll @T briefs :: M [T]` — delegate to one sub-answerer per brief, ",
            "answered together as a batch `[T]` (`import Tidepool.Fork`). A forked child ",
            "may fork further, recursively — the driver enforces depth and total-descendant ",
            "budgets and refuses loudly past them. `T` may be any type in scope, including one you ",
            "declared yourself earlier this session — the child resolves it the same way.\n",
            "When `Green` is in your row, `async (fork @T brief)` (`Tidepool.Async`) ",
            "parks the fork in a green thread — spawn several, then `wait` each, ",
            "in the SAME block.",
        ]),
        type_params: &[],
        default_row_args: &[],
        helpers_row_polymorphic: true,
        extra_imports: &["import Tidepool.Fork"],
        type_defs: Vec::new(),
        foreign_types: &[],
        errors: None,
        verbs: vec![
            Verb {
                ctor: "ForkWith",
                method: "fork_with",
                args: vec![
                    site_arg(),
                    Arg {
                        name: "brief",
                        ty: HsType::Text,
                        rust: RustBinding::Derived,
                    },
                ],
                ret: HsType::Value,
                errors: None,
                handling: HandlingClass::Fork,
                extract: None,
            },
            Verb {
                ctor: "ForkAllWith",
                method: "fork_all_with",
                args: vec![
                    site_arg(),
                    Arg {
                        name: "prompts",
                        ty: HsType::list(HsType::Text),
                        rust: RustBinding::Derived,
                    },
                ],
                ret: HsType::Value,
                errors: None,
                handling: HandlingClass::Fork,
                extract: None,
            },
        ],
        helpers: vec![
            Helper {
                name: "forkSited",
                ctor: Some("ForkWith"),
                doc: &[],
                substrate: true,
                body: HelperBody::OpaqueSited {
                    tyvars: &["a"],
                    site_param: "sid",
                    params: vec![Arg {
                        name: "brief",
                        ty: HsType::Text,
                        rust: RustBinding::Derived,
                    }],
                    ctor_args: vec![SitedCtorArg::Site, SitedCtorArg::Param("brief")],
                    coerce: true,
                    ret: HsType::Var("a"),
                },
            },
            Helper {
                name: "forkAllSited",
                ctor: Some("ForkAllWith"),
                doc: &[],
                substrate: true,
                body: HelperBody::OpaqueSited {
                    tyvars: &["a"],
                    site_param: "sid",
                    params: vec![Arg {
                        name: "prompts",
                        ty: HsType::list(HsType::Text),
                        rust: RustBinding::Derived,
                    }],
                    ctor_args: vec![SitedCtorArg::Site, SitedCtorArg::Param("prompts")],
                    coerce: true,
                    ret: HsType::list(HsType::Var("a")),
                },
            },
        ],
        // Both constructors return concrete `Value` — the `fork @T`/`forkAll
        // @T` polymorphism lives entirely in `forkSited`/`forkAllSited`'s
        // `unsafeCoerce`-marshaling signatures above (`Tidepool.Effects`, not
        // this GADT). `None` here is correct, not a placeholder: nothing at
        // the GADT/row level binds at an invocation site.
        polymorphism: Polymorphism::None,
        // No real `tidepool-handlers` handler (see the module doc).
        dispatched: false,
    }
}
