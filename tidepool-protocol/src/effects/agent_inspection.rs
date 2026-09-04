//! Exact-incarnation lifecycle inspection capability.

use crate::hs::HsType;
use crate::schema::{Arg, Effect, HandlingClass, Polymorphism, RustBinding, Verb};

#[must_use]
pub fn agent_inspection() -> Effect {
    Effect {
        name: "AgentInspection",
        authored_surface: crate::schema::AuthoredSurface::OPAQUE,
        handler: "AgentInspectionDecodeHandler",
        handler_module: "agent_inspection",
        req_enum: "AgentInspectionReq",
        decl_fn: "agent_inspection_decl",
        description: &["Private exact-incarnation lifecycle inspection substrate."],
        prompt_card: None,
        type_params: &[],
        default_row_args: &[],
        helpers_row_polymorphic: true,
        extra_imports: &[],
        type_defs: Vec::new(),
        foreign_types: &[("ActorTerminalStatus", "crate::ActorTerminalStatusWire")],
        errors: None,
        verbs: vec![Verb {
            ctor: "AgentInspectWith",
            method: "agent_inspect_with",
            args: vec![Arg {
                name: "actor",
                ty: HsType::Tuple(vec![HsType::Int, HsType::Int]),
                rust: RustBinding::Path("(i64, i64)"),
            }],
            ret: HsType::maybe(HsType::Named("ActorTerminalStatus")),
            errors: None,
            handling: HandlingClass::Actor,
            extract: None,
        }],
        helpers: Vec::new(),
        polymorphism: Polymorphism::None,
        dispatched: false,
    }
}
