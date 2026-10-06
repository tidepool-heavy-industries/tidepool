//! The `Green` suspension — decode-only.
//!
//! `Tidepool.Async`'s substrate (`AsyncSpawnWith`/`AsyncDoneWith`/
//! `AsyncJoinAnyWith`/`AsyncStatusWith`/`AsyncCancelWith`).
//! Routed by CONSTRUCTOR NAME only, using the schema-owned roster:
//! `classify_hole` never decodes a Green verb's payload (`AsyncSpawnWith`'s
//! body field carries a live closure), so every
//! live payload field beyond a bare `Int` is bound as
//! [`crate::schema::RustBinding::HaskellValue`] — recognition, not
//! interpretation; the real decode happens at
//! `SelfHarnessDriver::service_green_hole`.
//!
//! All four substrate helpers below back `Tidepool.Async` (authored library
//! code, not generated here) and are `Member`-polymorphic. `AsyncSpawnWith`
//! existentially packages the spawned body's concrete row; Rust treats that
//! closure as an opaque live value and resumes it in the originating machine.

use crate::hs::HsType;
use crate::schema::{
    Arg, Effect, HandlingClass, Helper, HelperBody, JsonInstance, Polymorphism, RustBinding,
    SumVariant, TypeDef, TypeShape, Verb, WireDerive, WireDerives,
};

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

/// The `Green` suspension (all five `Async*With` verbs), decode-only.
#[must_use]
pub fn green() -> Effect {
    Effect {
        name: "Green",
        authored_surface: crate::schema::AuthoredSurface::All,
        handler: "GreenDecodeHandler",
        handler_module: "green",
        req_enum: "GreenReq",
        decl_fn: "green_decl",
        description: &[
            "Cooperative concurrency for independent effectful computations within one invocation, using the `Control.Concurrent.Async` vocabulary through `Tidepool.Async`. ",
            "`async`/`wait`, `concurrently`, `race`, and `mapConcurrently` overlap waits on effects; ordinary `traverse` sequences an `Eff` computation and is not a concurrency operator. ",
            "Threads are parked continuations, not CPU-parallel workers: a computation that never yields through an effect can starve siblings. ",
            "`waitEither` observes the first result and leaves the other thread running; `race` cancels the losing thread. ",
            "Model domain failures with `Either` or another result ADT; `waitCatch` reports cancellation, not arbitrary exceptions. ",
            "`cancel` closes that thread's runtime resource scope and discards its pending suspensions; independently owned external work is settled by its own owner. ",
            "Spawn and join within the same invocation. Use actor-owned responses and watches when work must outlive the cell. Raw constructors here are substrate; authors use `Tidepool.Async`.",
        ],
        prompt_card: Some(&[
            "`Tidepool.Async` provides cooperative, invocation-scoped concurrency: `concurrently` for all results, `race` for the first result with loser cancellation, `async`/`wait` for explicit joins. ",
            "Pure computation is not preempted; concurrency overlaps effect waits. Use `Either` for domain failure and `waitCatch` for cancellation. ",
            "Join before the cell ends; retain successful results with `<-`. Use persistent child lifetimes and watches for work spanning model turns.",
        ]),
        type_params: &[],
        default_row_args: &[],
        extra_imports: &["import Tidepool.Async"],
        type_defs: vec![TypeDef {
            name: "AsyncStatus",
            wire_rust: None,
            haskell_module: None,
            shape: TypeShape::Sum {
                variants: vec![
                    SumVariant {
                        ctor: "AsyncRunning",
                        fields: positional_fields![],
                        doc: &[],
                    },
                    SumVariant {
                        ctor: "AsyncSettled",
                        fields: positional_fields![],
                        doc: &[],
                    },
                    SumVariant {
                        ctor: "AsyncWasCancelled",
                        fields: positional_fields![],
                        doc: &[],
                    },
                ],
            },
            json: JsonInstance::None,
            derives: WireDerives(&[
                WireDerive::Debug,
                WireDerive::Clone,
                WireDerive::PartialEq,
                WireDerive::Eq,
            ]),
            domain: None,
            doc: &[],
        }],
        external_types: &[],
        errors: None,
        verbs: vec![
            Verb {
                ctor: "AsyncSpawnWith",
                method: "async_spawn_with",
                args: vec![
                    site_arg(),
                    Arg {
                        name: "body",
                        // The body row is existential: construction chooses
                        // the caller's row, while the generated Core GADT stays
                        // independent of any authored effect-row synonym.
                        ty: HsType::func(
                            HsType::Int,
                            HsType::app(
                                HsType::app(HsType::Named("Eff"), HsType::Var("bodyEffs")),
                                HsType::Unit,
                            ),
                        ),
                        rust: RustBinding::HaskellValue,
                    },
                ],
                ret: HsType::Int,
                errors: None,
                handling: HandlingClass::Green,
            },
            Verb {
                ctor: "AsyncDoneWith",
                method: "async_done_with",
                args: vec![site_arg()],
                ret: HsType::Unit,
                errors: None,
                handling: HandlingClass::Green,
            },
            Verb {
                ctor: "AsyncJoinAnyWith",
                method: "async_join_any_with",
                args: vec![Arg {
                    name: "threadIds",
                    ty: HsType::list(HsType::Int),
                    rust: RustBinding::Derived,
                }],
                ret: HsType::Int,
                errors: None,
                handling: HandlingClass::Green,
            },
            Verb {
                ctor: "AsyncStatusWith",
                method: "async_status_with",
                args: vec![thread_id_arg()],
                ret: HsType::Int,
                errors: None,
                handling: HandlingClass::Green,
            },
            Verb {
                ctor: "AsyncCancelWith",
                method: "async_cancel_with",
                args: vec![thread_id_arg()],
                ret: HsType::Unit,
                errors: None,
                handling: HandlingClass::Green,
            },
        ],
        helpers: vec![
            Helper {
                name: "asyncSpawn",
                ctor: Some("AsyncSpawnWith"),
                doc: &[
                    "Fork a green thread; substrate for 'Tidepool.Async.async'.",
                    "The body rides as a lambda so the closure-sentinel scan fires",
                    "and the runtime tenures it (see the effect's Rust definition).",
                    "The authored wrapper publishes its result into a managed",
                    "Haskell cell before the body's last, payload-free",
                    "AsyncDoneWith suspension.",
                ],
                substrate: true,
                body: HelperBody::AsyncSpawnBody {
                    param: "body",
                    done_ctor: "AsyncDoneWith",
                },
            },
            Helper {
                name: "asyncJoinAny",
                ctor: Some("AsyncJoinAnyWith"),
                doc: &[
                    "Park until ANY of these threads reaches a terminal state;",
                    "resumes with the id of the one that did.",
                ],
                substrate: true,
                body: HelperBody::Pointfree,
            },
            Helper {
                name: "asyncStatus",
                ctor: Some("AsyncStatusWith"),
                doc: &["A thread's current state. Never parks."],
                substrate: true,
                body: HelperBody::IntDecode {
                    params: &["t"],
                    cases: &[(1, "AsyncSettled"), (2, "AsyncWasCancelled")],
                    default: "AsyncRunning",
                    result_type: "AsyncStatus",
                },
            },
            Helper {
                name: "asyncCancel",
                ctor: Some("AsyncCancelWith"),
                doc: &[
                    "Cancel a thread: its runtime resource scope closes, discarding its pending",
                    "suspensions. Idempotent, and a no-op on a terminal thread.",
                ],
                substrate: true,
                body: HelperBody::Pointfree,
            },
        ],
        // The substrate is monomorphic; the authored `Async a` wrapper owns
        // result polymorphism in Haskell-managed storage.
        polymorphism: Polymorphism::None,
        // There is deliberately no `GreenHandler` (see the module doc).
        generated_handler: false,
        handler_execution: crate::schema::HandlerExecution::Immediate,
        caller_principal: false,
    }
}
