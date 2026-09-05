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
        helpers_row_polymorphic: true,
        extra_imports: &[],
        type_defs: vec![
            sum(
                "AgentStopControlOutcome",
                vec![
                    variant("AgentStoppedNow", vec![]),
                    variant("AgentStopAlreadyStopped", vec![]),
                    variant("AgentStopUnavailable", vec![]),
                    variant("AgentStopUnauthorized", vec![]),
                    variant("AgentStopFailed", vec![HsType::Text]),
                ],
                &["Supervisor-owned retirement outcome for one exact actor incarnation."],
            ),
            sum(
                "CleanupActorState",
                vec![
                    variant("CleanupActorRunning", vec![]),
                    variant("CleanupActorTerminal", vec![]),
                ],
                &[],
            ),
            record(
                "CleanupActorPlan",
                vec![
                    field("cleanupActorId", HsType::Int),
                    field("cleanupActorIncarnation", HsType::Int),
                    field("cleanupActorLabel", HsType::Text),
                    field("cleanupActorState", HsType::Named("CleanupActorState")),
                    field("cleanupActorRevision", HsType::Int),
                ],
                &[],
            ),
            record(
                "CleanupPlan",
                vec![
                    field("cleanupPlanGroup", HsType::Int),
                    field(
                        "cleanupPlanActors",
                        HsType::list(HsType::Named("CleanupActorPlan")),
                    ),
                    field("cleanupPlanPendingResponses", HsType::list(HsType::Int)),
                    field("cleanupPlanPendingWatches", HsType::list(HsType::Int)),
                    field("cleanupPlanRefusal", HsType::maybe(HsType::Text)),
                ],
                &["A read-only, deepest-first campaign cleanup projection."],
            ),
            sum(
                "CleanupStepReceipt",
                vec![
                    variant("CleanupForgotResponses", vec![HsType::list(HsType::Int)]),
                    variant("CleanupForgotWatches", vec![HsType::list(HsType::Int)]),
                    variant(
                        "CleanupStoppedActor",
                        vec![
                            HsType::Int,
                            HsType::Int,
                            HsType::Named("AgentStopControlOutcome"),
                        ],
                    ),
                    variant("CleanupForgotActor", vec![HsType::Int, HsType::Int]),
                    variant(
                        "CleanupActorRetained",
                        vec![
                            HsType::Int,
                            HsType::Int,
                            HsType::list(HsType::Int),
                            HsType::list(HsType::Int),
                        ],
                    ),
                    variant("CleanupGroupRetired", vec![HsType::Int]),
                    variant("CleanupBlocked", vec![HsType::Text]),
                    variant("CleanupStalePlan", vec![]),
                ],
                &[],
            ),
            record(
                "CleanupReceipt",
                vec![
                    field("cleanupReceiptPlan", HsType::Named("CleanupPlan")),
                    field(
                        "cleanupReceiptSteps",
                        HsType::list(HsType::Named("CleanupStepReceipt")),
                    ),
                    field("cleanupReceiptComplete", HsType::Bool),
                ],
                &["Refusal-bearing receipt for one idempotent cleanup attempt."],
            ),
        ],
        foreign_types: &[],
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
                extract: None,
            },
            Verb {
                ctor: "AgentControlExecuteCleanupWith",
                method: "agent_control_execute_cleanup_with",
                args: vec![
                    Arg {
                        name: "group",
                        ty: HsType::Int,
                        rust: RustBinding::Path("i64"),
                    },
                    Arg {
                        name: "inspected",
                        ty: HsType::list(HsType::Tuple(vec![
                            HsType::Int,
                            HsType::Int,
                            HsType::Int,
                        ])),
                        rust: RustBinding::Path("Vec<(i64, i64, i64)>"),
                    },
                ],
                ret: HsType::Named("CleanupReceipt"),
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
        shape: TypeShape::Sum { variants },
        json: JsonInstance::None,
        derives: NO_WIRE,
        domain: None,
        doc,
    }
}

fn record(
    name: &'static str,
    fields: Vec<crate::schema::RecordField>,
    doc: &'static [&'static str],
) -> TypeDef {
    TypeDef {
        name,
        wire_rust: None,
        shape: TypeShape::Record { fields },
        json: JsonInstance::None,
        derives: NO_WIRE,
        domain: None,
        doc,
    }
}

fn field(name: &'static str, ty: HsType) -> crate::schema::RecordField {
    crate::schema::RecordField {
        hs_name: name,
        rust_name: name,
        ty,
        doc: &[],
    }
}
