//! The actor-runtime suspension boundary.
//!
//! The public Haskell API lives in `Tidepool.Actor`; this schema owns only the
//! one internal request needed by its first vertical slice.  A wait carries an
//! exact Rust routing identity and returns terminal metadata.  The successful
//! exit value never crosses this request: it remains in the managed Haskell
//! cell carried by the corresponding `AgentRef`.
//!
//! There is no `tidepool-handlers` handler.  The request is decoded and
//! serviced by `tidepool-actor`, whose registry owns exact-incarnation wait
//! semantics.

use crate::hs::HsType;
use crate::schema::{
    Arg, Effect, HandlingClass, JsonInstance, Polymorphism, RustBinding, SumVariant, TypeDef,
    TypeShape, Verb, WireDerive, WireDerives,
};

/// The actor effect's internal wait request.
#[must_use]
pub fn actor() -> Effect {
    Effect {
        name: "Actor",
        handler: "ActorDecodeHandler",
        handler_module: "actor",
        req_enum: "ActorReq",
        decl_fn: "actor_decl",
        description: &[
            "Typed actor lifecycle and communication. Authored code uses `Tidepool.Actor`; ",
            "the constructors in this effect are runtime substrate, not a second public API.",
        ],
        prompt_card: Some(&[
            "`Tidepool.Actor` provides exact-incarnation actor references and typed exit ",
            "observation. `awaitExit ref` returns `Completed value`, `Failed reason`, or ",
            "`Cancelled reason`; it is repeatable and preserves closure-valued exits.",
        ]),
        type_params: &[],
        default_row_args: &[],
        helpers_row_polymorphic: true,
        extra_imports: &["import Tidepool.Actor"],
        type_defs: vec![TypeDef {
            name: "ActorTerminalStatus",
            wire_rust: None,
            shape: TypeShape::Sum {
                variants: vec![
                    SumVariant {
                        ctor: "ActorCompletedStatus",
                        fields: vec![],
                        doc: &[],
                    },
                    SumVariant {
                        ctor: "ActorFailedStatus",
                        fields: vec![HsType::Text],
                        doc: &[],
                    },
                    SumVariant {
                        ctor: "ActorCancelledStatus",
                        fields: vec![HsType::Text],
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
        verbs: vec![Verb {
            ctor: "ActorWaitWith",
            method: "actor_wait_with",
            args: vec![
                Arg {
                    name: "actorId",
                    ty: HsType::Int,
                    rust: RustBinding::Derived,
                },
                Arg {
                    name: "incarnation",
                    ty: HsType::Int,
                    rust: RustBinding::Derived,
                },
            ],
            ret: HsType::Named("ActorTerminalStatus"),
            errors: None,
            handling: HandlingClass::Actor,
            extract: None,
        }],
        // The public wrapper needs the managed cell carried by `AgentRef`, so
        // it is authored in Tidepool.Actor rather than emitted as a second,
        // raw helper here.
        helpers: Vec::new(),
        polymorphism: Polymorphism::None,
        dispatched: false,
    }
}
