//! The actor-runtime suspension boundary.
//!
//! The public Haskell API lives in `Tidepool.Actor`; this schema owns only the
//! outward requests needed by the first actor vertical. A start carries one
//! existentially row-typed child entry as its field-1 live payload. A wait
//! carries an exact Rust routing identity and returns terminal metadata. The
//! successful exit value never crosses either request: it remains in the
//! managed Haskell cell carried by the corresponding `ActorRef`.
//!
//! There is no `tidepool-handlers` handler.  The request is decoded and
//! serviced by `tidepool-actor`, whose registry owns exact-incarnation wait
//! semantics.

use crate::hs::HsType;
use crate::schema::{
    Arg, Effect, HandlingClass, JsonInstance, Polymorphism, RustBinding, SumVariant, TypeDef,
    TypeShape, Verb, WireDerive, WireDerives,
};

fn address_type() -> HsType {
    HsType::Tuple(vec![HsType::Int, HsType::Int])
}

/// The actor effect's internal start and wait requests.
#[must_use]
pub fn actor() -> Effect {
    Effect {
        name: "Actor",
        authored_surface: crate::schema::AuthoredSurface::OPAQUE,
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
        verbs: vec![
            Verb {
                ctor: "ActorPromoteWith",
                method: "actor_promote_with",
                args: vec![],
                ret: HsType::Text,
                errors: None,
                handling: HandlingClass::Actor,
                extract: None,
            },
            Verb {
                ctor: "ActorStartWith",
                method: "actor_start_with",
                args: vec![
                    Arg {
                        name: "label",
                        ty: HsType::Text,
                        rust: RustBinding::Derived,
                    },
                    Arg {
                        name: "entry",
                        ty: HsType::func(
                            HsType::Int,
                            HsType::app(
                                HsType::app(HsType::Named("Eff"), HsType::Var("childEffs")),
                                HsType::Unit,
                            ),
                        ),
                        rust: RustBinding::CoreValue,
                    },
                    Arg {
                        name: "promotion",
                        ty: HsType::Text,
                        rust: RustBinding::Derived,
                    },
                ],
                ret: address_type(),
                errors: None,
                handling: HandlingClass::Actor,
                extract: None,
            },
            Verb {
                ctor: "ActorWaitWith",
                method: "actor_wait_with",
                args: vec![Arg {
                    name: "actor",
                    ty: address_type(),
                    rust: RustBinding::Path("(i64, i64)"),
                }],
                ret: HsType::Named("ActorTerminalStatus"),
                errors: None,
                handling: HandlingClass::Actor,
                extract: None,
            },
        ],
        // The public wrapper needs the managed cell carried by `ActorRef`, so
        // it is authored in Tidepool.Actor rather than emitted as a second,
        // raw helper here.
        helpers: Vec::new(),
        polymorphism: Polymorphism::None,
        dispatched: false,
    }
}
