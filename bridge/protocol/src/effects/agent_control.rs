//! Supervised actor-control capability for the interactive facade.

use crate::hs::HsType;
use crate::schema::{
    Arg, Effect, HandlingClass, JsonInstance, Polymorphism, RustBinding, SumVariant, TypeDef,
    TypeShape, VariantFields, Verb, WireDerives,
};

const NO_WIRE: WireDerives = WireDerives(&[]);

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
        extra_imports: &[],
        type_defs: vec![
            sum(
                "AgentStopControlOutcome",
                vec![
                    variant("AgentStoppedNow", vec![]),
                    variant("AgentStoppedRetaining", vec![HsType::Text]),
                    variant("AgentStoppedReleasing", vec![]),
                    variant("AgentStopAlreadyStopped", vec![]),
                    variant("AgentStopUnavailable", vec![]),
                    variant("AgentStopUnauthorized", vec![]),
                    variant("AgentStopFailed", vec![HsType::Text]),
                ],
                &[
                    "Supervisor-owned retirement outcome for one exact actor incarnation.",
                    "AgentStoppedNow: the actor is stopped and its host resources are released.",
                    "AgentStoppedRetaining: stopped, but the named resources stay retained.",
                    "AgentStoppedReleasing: stopped; release had not settled and a notice follows.",
                ],
            ),
            sum("AgentRetentionError", vec![
                variant("AgentRetainUnavailable", vec![]),
                variant("AgentRetainUnauthorized", vec![]),
                variant("AgentRetainOwnerUnavailable", vec![]),
                variant("AgentRetainOwnerClosed", vec![]),
            ], &["Refusal to transfer an actor's cleanup ownership."]),
        ],
        external_types: &[crate::schema::ExternalType {
            haskell_name: "WorkerLifetime",
            rust_wire: "crate::WorkerLifetime",
            core_module: None,
        }],
        errors: None,
        verbs: vec![
            Verb {
                ctor: "AgentControlStopWith",
                method: "agent_control_stop_with",
                args: vec![Arg {
                    name: "actor",
                    ty: HsType::Tuple(vec![HsType::Int, HsType::Int]),
                    rust: RustBinding::Path("(i64, i64)"),
                }],
                ret: HsType::Named("AgentStopControlOutcome"),
                errors: None,
                handling: HandlingClass::Actor,
            },
            Verb {
                ctor: "AgentControlRetainWith",
                method: "agent_control_retain_with",
                args: vec![
                    Arg { name: "actor", ty: HsType::Tuple(vec![HsType::Int, HsType::Int]), rust: RustBinding::Derived },
                    Arg { name: "lifetime", ty: HsType::Named("WorkerLifetime"), rust: RustBinding::External },
                ],
                ret: HsType::either(HsType::Named("AgentRetentionError"), HsType::Unit),
                errors: None,
                handling: HandlingClass::Actor,
            },
        ],
        helpers: Vec::new(),
        polymorphism: Polymorphism::None,
        generated_handler: false,
        handler_execution: crate::schema::HandlerExecution::Immediate,
        caller_principal: false,
    }
}

fn variant(ctor: &'static str, fields: Vec<HsType>) -> SumVariant {
    SumVariant {
        ctor,
        fields: VariantFields::Positional(fields),
        doc: &[],
    }
}

fn sum(name: &'static str, variants: Vec<SumVariant>, doc: &'static [&'static str]) -> TypeDef {
    TypeDef {
        name,
        wire_rust: None,
        haskell_module: None,
        shape: TypeShape::Sum { variants },
        json: JsonInstance::None,
        derives: NO_WIRE,
        domain: None,
        doc,
    }
}
