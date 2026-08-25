//! The `Green` suspension — decode-only.
//!
//! `Tidepool.Async`'s substrate (`AsyncSpawnWith`/`AsyncDoneWith`/
//! `AsyncJoinAnyWith`/`AsyncStatusWith`/`AsyncResultWith`/`AsyncCancelWith`).
//! Routed by CONSTRUCTOR NAME only, same discipline as [`crate::effects::subagent`]:
//! `classify_hole` never decodes a Green verb's payload (field 1 of
//! `AsyncSpawnWith`/`AsyncDoneWith` may carry a live closure), so every
//! payload field beyond a bare `Int` is bound as
//! [`crate::schema::RustBinding::CoreValue`] — recognition, not
//! interpretation; the real decode happens at
//! `SelfHarnessDriver::service_green_hole`.
//!
//! Arities transcribed from `tidepool-mcp/src/effect_defs.rs`'s
//! `green_effect_def!` verbs list. NOT in [`crate::effects::all`] — see
//! [`crate::effects::suspension_roster`].

use crate::hs::HsType;
use crate::schema::{Arg, Effect, HandlingClass, RustBinding, Verb};

fn site_arg() -> Arg {
    Arg {
        name: "site",
        ty: HsType::Int,
        rust: RustBinding::Derived,
    }
}

fn thread_id_arg() -> Arg {
    Arg {
        name: "threadId",
        ty: HsType::Int,
        rust: RustBinding::Derived,
    }
}

/// The `Green` suspension (all six `Async*With` verbs), decode-only.
#[must_use]
pub fn green() -> Effect {
    Effect {
        name: "Green",
        handler: "GreenDecodeHandler",
        handler_module: "green",
        req_enum: "GreenReq",
        decl_fn: "green_decl",
        description: &["Suspend to the driver's green-thread scheduler (decode-only schema)."],
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
                ctor: "AsyncSpawnWith",
                method: "async_spawn_with",
                args: vec![
                    site_arg(),
                    Arg {
                        name: "body",
                        ty: HsType::Value,
                        rust: RustBinding::CoreValue,
                    },
                ],
                ret: HsType::Unit,
                errors: None,
                handling: HandlingClass::Green,
                extract: None,
            },
            Verb {
                ctor: "AsyncDoneWith",
                method: "async_done_with",
                args: vec![
                    site_arg(),
                    Arg {
                        name: "value",
                        ty: HsType::Var("a"),
                        rust: RustBinding::CoreValue,
                    },
                ],
                ret: HsType::Unit,
                errors: None,
                handling: HandlingClass::Green,
                extract: None,
            },
            Verb {
                ctor: "AsyncJoinAnyWith",
                method: "async_join_any_with",
                args: vec![Arg {
                    name: "threadIds",
                    ty: HsType::list(HsType::Int),
                    rust: RustBinding::Derived,
                }],
                ret: HsType::Unit,
                errors: None,
                handling: HandlingClass::Green,
                extract: None,
            },
            Verb {
                ctor: "AsyncStatusWith",
                method: "async_status_with",
                args: vec![thread_id_arg()],
                ret: HsType::Unit,
                errors: None,
                handling: HandlingClass::Green,
                extract: None,
            },
            Verb {
                ctor: "AsyncResultWith",
                method: "async_result_with",
                args: vec![thread_id_arg()],
                ret: HsType::Unit,
                errors: None,
                handling: HandlingClass::Green,
                extract: None,
            },
            Verb {
                ctor: "AsyncCancelWith",
                method: "async_cancel_with",
                args: vec![thread_id_arg()],
                ret: HsType::Unit,
                errors: None,
                handling: HandlingClass::Green,
                extract: None,
            },
        ],
        helpers: Vec::new(),
    }
}
