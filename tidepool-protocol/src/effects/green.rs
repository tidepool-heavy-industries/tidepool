//! The `Green` suspension — decode-only.
//!
//! `Tidepool.Async`'s substrate (`AsyncSpawnWith`/`AsyncDoneWith`/
//! `AsyncJoinAnyWith`/`AsyncStatusWith`/`AsyncCancelWith`).
//! Routed by CONSTRUCTOR NAME only, same discipline as [`crate::effects::subagent`]:
//! `classify_hole` never decodes a Green verb's payload (`AsyncSpawnWith`'s
//! body field carries a live closure), so every
//! live payload field beyond a bare `Int` is bound as
//! [`crate::schema::RustBinding::CoreValue`] — recognition, not
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
            "Green threads: cooperative concurrency with the authored surface of ",
            "`Control.Concurrent.Async` (`Tidepool.Async`: `async`/`wait`/ ",
            "`waitEither`/`cancel`, plus `race`/`concurrently`/`mapConcurrently`). ",
            "A forked computation parks as its own continuation, so threads ",
            "blocked on different effects progress independently and several ",
            "holes are pending at once. Scheduling is cooperative — a thread runs ",
            "until it performs an effect, then parks, and the driver resumes ",
            "whichever pending hole is ready. `cancel` closes the thread\'s runtime ",
            "resource scope, which discards its pending suspensions. The verbs here are ",
            "substrate; authors call `Tidepool.Async`, not these.",
        ],
        prompt_card: Some(&[
            "`Tidepool.Async` — the `Control.Concurrent.Async` surface ",
            "(`async`/`wait`/`waitCatch`/`waitEither`/`waitBoth`/`waitAny`/`cancel`, ",
            "`race`/`concurrently`/`mapConcurrently`), auto-imported. ",
            "`async (fork @T brief)` runs a fork in a green thread so several can be ",
            "outstanding before the first `wait`. Spawn and wait in the SAME ",
            "```haskell block — threads do not survive their block; results you ",
            "bound with `<-` do.",
        ]),
        type_params: &[],
        default_row_args: &[],
        helpers_row_polymorphic: true,
        extra_imports: &["import Tidepool.Async"],
        type_defs: vec![TypeDef {
            name: "AsyncStatus",
            wire_rust: None,
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
                        // The body row is existential: construction chooses
                        // the caller's row, while the generated Core GADT stays
                        // independent of every per-window `M` synonym.
                        ty: HsType::func(
                            HsType::Int,
                            HsType::app(
                                HsType::app(HsType::Named("Eff"), HsType::Var("bodyEffs")),
                                HsType::Unit,
                            ),
                        ),
                        rust: RustBinding::CoreValue,
                    },
                ],
                ret: HsType::Int,
                errors: None,
                handling: HandlingClass::Green,
                extract: None,
            },
            Verb {
                ctor: "AsyncDoneWith",
                method: "async_done_with",
                args: vec![site_arg()],
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
                ret: HsType::Int,
                errors: None,
                handling: HandlingClass::Green,
                extract: None,
            },
            Verb {
                ctor: "AsyncStatusWith",
                method: "async_status_with",
                args: vec![thread_id_arg()],
                ret: HsType::Int,
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
        dispatched: false,
    }
}
