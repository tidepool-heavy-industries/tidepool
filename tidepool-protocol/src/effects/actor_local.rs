//! The actor-local lifecycle and mailbox suspension boundary.
//!
//! Unlike the outward `Actor` capability, this effect is indexed by the
//! current actor's protocol and successful-exit type. Its raw constructors are
//! kernel substrate; `Tidepool.Actor` exposes the authored operations.

use crate::schema::{Effect, HandlingClass, Polymorphism, TypeParam, Verb};
use crate::HsType;

const TYPE_PARAMS: &[TypeParam] = &[TypeParam::unary("api"), TypeParam::value("exit")];

/// The indexed actor-local effect.
#[must_use]
pub fn actor_local() -> Effect {
    Effect {
        name: "ActorLocal",
        handler: "ActorLocalDecodeHandler",
        handler_module: "actor_local",
        req_enum: "ActorLocalReq",
        decl_fn: "actor_local_decl",
        description: &[
            "Private actor-local lifecycle and mailbox substrate. Authored code uses ",
            "`Tidepool.Actor`; the protocol and exit indexes tie an installed program ",
            "to its exact actor incarnation.",
        ],
        prompt_card: None,
        type_params: TYPE_PARAMS,
        default_row_args: &["Maybe", "Void"],
        helpers_row_polymorphic: true,
        extra_imports: &["import Tidepool.Actor"],
        type_defs: Vec::new(),
        foreign_types: &[],
        errors: None,
        verbs: vec![Verb {
            ctor: "ActorReadyWith",
            method: "actor_ready_with",
            args: Vec::new(),
            ret: HsType::Unit,
            errors: None,
            handling: HandlingClass::Actor,
            extract: None,
        }],
        helpers: Vec::new(),
        polymorphism: Polymorphism::None,
        dispatched: false,
    }
}
