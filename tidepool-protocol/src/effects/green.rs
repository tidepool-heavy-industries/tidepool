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
//! All five substrate helpers below back `Tidepool.Async` (authored library
//! code, not generated here): `asyncJoinAny`/`asyncStatus`/`asyncResult`/
//! `asyncCancel` are thin `Member`-polymorphic wrappers; `asyncSpawn` is the
//! one exception to `helpers_row_polymorphic` in this whole schema, forced
//! concrete (`M`, not `Eff effs`) because `AsyncSpawnWith`'s own constructor
//! field type is fixed to `Int -> M ()` by the wire shape — see
//! [`crate::schema::HelperBody::AsyncSpawnBody`]'s own doc.

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

/// The `Green` suspension (all six `Async*With` verbs), decode-only.
#[must_use]
pub fn green() -> Effect {
    Effect {
        name: "Green",
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
                        fields: vec![],
                        doc: &[],
                    },
                    SumVariant {
                        ctor: "AsyncSettled",
                        fields: vec![],
                        doc: &[],
                    },
                    SumVariant {
                        ctor: "AsyncWasCancelled",
                        fields: vec![],
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
                        // `Int -> M ()`, not `Value`: the field genuinely is
                        // a function type (see `HsType::Fn`'s own doc), and
                        // the constructor signature must say so — a `Value`
                        // field here is what let `AsyncSpawnWith 0 (\_ -> …)`
                        // (a real lambda) silently mistype against the
                        // generated GADT. `Named("M ()")` stands in for `M`
                        // applied to `()`: the closed type language has no
                        // general type-application shape, and this is the
                        // one place in the whole schema that needs it — see
                        // `HsType::Fn`'s doc for why `M` (not `Eff effs`) is
                        // correct here.
                        ty: HsType::func(HsType::Int, HsType::Named("M ()")),
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
                ctor: "AsyncResultWith",
                method: "async_result_with",
                args: vec![thread_id_arg()],
                // Ordinary Hindley-Milner polymorphism inferred from the
                // thread body's own type, not `@T`-style invocation binding
                // (no `TypeApplications` call site for extract to pattern-
                // match on) — `Polymorphism::None` below is correct.
                ret: HsType::Var("a"),
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
                    "The body is wrapped so its last act is an AsyncDoneWith",
                    "suspension carrying the result — the return trip uses the",
                    "same field-1 crossing as the outbound one.",
                ],
                substrate: true,
                body: HelperBody::AsyncSpawnBody {
                    param: "body",
                    result_param: "v",
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
                name: "asyncResult",
                ctor: Some("AsyncResultWith"),
                doc: &[
                    "A settled thread's result, delivered in-heap by handle.",
                    "Gate it with 'asyncStatus': the result of a thread that has",
                    "not settled is not defined.",
                ],
                substrate: true,
                body: HelperBody::Pointfree,
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
        // `AsyncDoneWith`/`AsyncResultWith`'s `Var("a")` usage is ordinary
        // inferred polymorphism (from `asyncSpawn :: M a -> M Int`'s own
        // argument), not an `@T` invocation-site binding — no call site
        // applies a type argument here. `None` is correct.
        polymorphism: Polymorphism::None,
        // There is deliberately no `GreenHandler` (see the module doc).
        dispatched: false,
    }
}
