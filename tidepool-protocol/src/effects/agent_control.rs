//! Supervised actor-control capability for the interactive facade.

use crate::hs::HsType;
use crate::schema::{Arg, Effect, HandlingClass, Polymorphism, RustBinding, Verb};

#[must_use]
pub fn agent_control() -> Effect {
    Effect {
        name: "AgentControl",
        authored_surface: crate::schema::AuthoredSurface::OPAQUE,
        handler: "AgentControlDecodeHandler",
        handler_module: "agent_control",
        req_enum: "AgentControlReq",
        decl_fn: "agent_control_decl",
        description: &["Private supervised actor-control substrate used by typed stop operations."],
        prompt_card: None,
        type_params: &[],
        default_row_args: &[],
        helpers_row_polymorphic: true,
        extra_imports: &[],
        type_defs: Vec::new(),
        foreign_types: &[("ActorCallStatus", "crate::ActorCallStatusWire")],
        errors: None,
        verbs: vec![Verb {
            ctor: "AgentControlTryCallWith",
            method: "agent_control_try_call_with",
            args: vec![
                Arg {
                    name: "actor",
                    ty: HsType::Tuple(vec![HsType::Int, HsType::Int]),
                    rust: RustBinding::Path("(i64, i64)"),
                },
                Arg {
                    name: "request",
                    ty: HsType::app(HsType::Var("protocol"), HsType::Unit),
                    rust: RustBinding::CoreValue,
                },
            ],
            ret: HsType::Named("ActorCallStatus"),
            errors: None,
            handling: HandlingClass::Actor,
            extract: None,
        }],
        helpers: Vec::new(),
        polymorphism: Polymorphism::None,
        dispatched: false,
    }
}
