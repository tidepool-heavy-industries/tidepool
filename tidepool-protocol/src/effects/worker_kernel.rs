//! Private worker-ledger suspension boundary.
//!
//! The typed DevSwarm facade hides these JSON-shaped requests. Rust interprets
//! them to own idempotency, actor correlation, candidate receipts, and
//! acknowledgement without exposing a copied registry value to Haskell.

use crate::hs::HsType;
use crate::schema::{Arg, Effect, HandlingClass, Polymorphism, RustBinding, Verb};

fn json_arg(name: &'static str) -> Arg {
    Arg {
        name,
        ty: HsType::Value,
        rust: RustBinding::CoreValue,
    }
}

fn text_arg(name: &'static str) -> Arg {
    Arg {
        name,
        ty: HsType::Text,
        rust: RustBinding::Derived,
    }
}

#[must_use]
pub fn worker_kernel() -> Effect {
    Effect {
        name: "WorkerKernel",
        authored_surface: crate::schema::AuthoredSurface::OPAQUE,
        handler: "WorkerKernelDecodeHandler",
        handler_module: "worker_kernel",
        req_enum: "WorkerKernelReq",
        decl_fn: "worker_kernel_decl",
        description: &[
            "Private interpreter boundary for Rust-owned worker lifecycle and custody. ",
            "Authored code uses the typed DevSwarm facade, never these constructors.",
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
                ctor: "WorkerReserveBatchWith",
                method: "worker_reserve_batch_with",
                args: vec![json_arg("specs")],
                ret: HsType::Value,
                errors: None,
                handling: HandlingClass::Actor,
                extract: None,
            },
            Verb {
                ctor: "WorkerAttachWith",
                method: "worker_attach_with",
                args: vec![
                    text_arg("handle"),
                    Arg {
                        name: "actor",
                        ty: HsType::Tuple(vec![HsType::Int, HsType::Int]),
                        rust: RustBinding::Path("(i64, i64)"),
                    },
                    text_arg("worktree"),
                ],
                ret: HsType::Value,
                errors: None,
                handling: HandlingClass::Actor,
                extract: None,
            },
            Verb {
                ctor: "WorkerFailStartWith",
                method: "worker_fail_start_with",
                args: vec![text_arg("handle"), text_arg("detail")],
                ret: HsType::Value,
                errors: None,
                handling: HandlingClass::Actor,
                extract: None,
            },
            Verb {
                ctor: "WorkerSubmitWith",
                method: "worker_submit_with",
                args: vec![text_arg("handle"), json_arg("receipt")],
                ret: HsType::Unit,
                errors: None,
                handling: HandlingClass::Actor,
                extract: None,
            },
            Verb {
                ctor: "WorkerListWith",
                method: "worker_list_with",
                args: vec![],
                ret: HsType::Value,
                errors: None,
                handling: HandlingClass::Actor,
                extract: None,
            },
            Verb {
                ctor: "WorkerCollectWith",
                method: "worker_collect_with",
                args: vec![json_arg("handles")],
                ret: HsType::Value,
                errors: None,
                handling: HandlingClass::Actor,
                extract: None,
            },
            Verb {
                ctor: "WorkerAcknowledgeWith",
                method: "worker_acknowledge_with",
                args: vec![json_arg("acknowledgements")],
                ret: HsType::Value,
                errors: None,
                handling: HandlingClass::Actor,
                extract: None,
            },
            Verb {
                ctor: "WorkerSessionContextWith",
                method: "worker_session_context_with",
                args: vec![],
                ret: HsType::Value,
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
