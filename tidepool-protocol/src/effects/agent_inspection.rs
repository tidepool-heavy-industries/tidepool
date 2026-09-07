//! Exact-incarnation lifecycle inspection capability.

use crate::hs::HsType;
use crate::schema::{
    Arg, Effect, HandlingClass, JsonInstance, Polymorphism, RecordField, RustBinding, SumVariant,
    TypeDef, TypeShape, VariantFields, Verb, WireDerives,
};

const NO_WIRE: WireDerives = WireDerives(&[]);

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
        type_defs: vec![
            TypeDef {
                name: "ProviderFailureKind", wire_rust: None,
                shape: TypeShape::Sum { variants: vec![variant("RequestRejected", vec![]),
                    variant("TransportFailed", vec![]), variant("OtherProviderFailure", vec![HsType::Text])] },
                json: JsonInstance::None, derives: NO_WIRE, domain: None, doc: &[],
            },
            TypeDef {
                name: "ProviderHealth", wire_rust: None,
                shape: TypeShape::Sum { variants: vec![
                    variant("ProviderUnknown", vec![]), variant("ProviderActive", vec![]),
                    variant("ProviderSucceeded", vec![]), variant("ProviderInterrupted", vec![]),
                    variant("ProviderFailed", vec![HsType::Named("ProviderFailureKind")]),
                ] }, json: JsonInstance::None, derives: NO_WIRE, domain: None, doc: &[],
            },
            TypeDef {
                name: "AgentDisposition", wire_rust: None,
                shape: TypeShape::Sum { variants: vec![
                    variant("Working", vec![]), variant("NeedsAttention", vec![]),
                    variant("SettledAwaitingProvider", vec![]), variant("IdleRetained", vec![]),
                ] }, json: JsonInstance::None, derives: NO_WIRE, domain: None,
                doc: &["Derived request/provider posture; only IdleRetained is a retirement candidate."],
            },
            TypeDef {
                name: "CacheBoundaryReason",
                wire_rust: None,
                shape: TypeShape::Sum {
                    variants: vec![
                        variant("CacheFresh", vec![]),
                        variant("CacheForkedPrefix", vec![]),
                        variant("CacheReattachedThread", vec![]),
                        variant("CacheProviderUnknown", vec![]),
                    ],
                },
                json: JsonInstance::None,
                derives: NO_WIRE,
                domain: None,
                doc: &["Why Tidepool expected this provider context boundary."],
            },
            TypeDef {
                name: "AgentRosterState",
                wire_rust: None,
                shape: TypeShape::Sum {
                    variants: vec![
                        variant("RosterRunning", vec![]),
                        variant("RosterStopped", vec![]),
                        variant("RosterFailed", vec![HsType::Text]),
                        variant("RosterCancelled", vec![HsType::Text]),
                    ],
                },
                json: JsonInstance::None,
                derives: NO_WIRE,
                domain: None,
                doc: &[],
            },
            TypeDef {
                name: "AgentWorkbenchTransfer",
                wire_rust: None,
                shape: TypeShape::Sum {
                    variants: vec![
                        variant("WorkbenchReplyTransfer", vec![]),
                        variant("WorkbenchCancellationTransfer", vec![]),
                    ],
                },
                json: JsonInstance::None,
                derives: NO_WIRE,
                domain: None,
                doc: &["An accepted terminal control transfer from the resident workbench."],
            },
            TypeDef {
                name: "AgentWorkbenchPosture",
                wire_rust: None,
                shape: TypeShape::Sum {
                    variants: vec![
                        variant("WorkbenchIdle", vec![]),
                        variant("WorkbenchRunningUnit", vec![HsType::Int, HsType::Int]),
                        variant(
                            "WorkbenchAwaitingEffect",
                            vec![HsType::Int, HsType::Int, HsType::Text],
                        ),
                        variant(
                            "WorkbenchTerminalTransfer",
                            vec![HsType::Named("AgentWorkbenchTransfer")],
                        ),
                        variant("WorkbenchFailed", vec![]),
                    ],
                },
                json: JsonInstance::None,
                derives: NO_WIRE,
                domain: None,
                doc: &[
                    "Whether hosted Haskell is idle, running, suspended at an effect, or terminal.",
                ],
            },
            TypeDef {
                name: "AgentRosterEntry",
                wire_rust: None,
                shape: TypeShape::Record {
                    fields: vec![
                        field("rosterActorId", HsType::Int),
                        field("rosterActorIncarnation", HsType::Int),
                        field("rosterLabel", HsType::Text),
                        field("rosterRequestedModel", HsType::maybe(HsType::Text)),
                        field("rosterConfirmedModel", HsType::maybe(HsType::Text)),
                        field("rosterReceivedRequests", HsType::Int),
                        field("rosterReceivedCoordinationEvents", HsType::Int),
                        field("rosterCompactions", HsType::maybe(HsType::Int)),
                        field("rosterSupervisorId", HsType::maybe(HsType::Int)),
                        field("rosterSupervisorIncarnation", HsType::maybe(HsType::Int)),
                        field("rosterContextParentId", HsType::maybe(HsType::Int)),
                        field("rosterContextParentIncarnation", HsType::maybe(HsType::Int)),
                        field("rosterState", HsType::Named("AgentRosterState")),
                        field("rosterProviderHealth", HsType::Named("ProviderHealth")),
                        field("rosterProviderTurn", HsType::maybe(HsType::Text)),
                        field("rosterProviderObservationStale", HsType::Bool),
                        field("rosterDisposition", HsType::maybe(HsType::Named("AgentDisposition"))),
                        field("rosterCurrentRequests", HsType::list(HsType::Int)),
                        field("rosterQueuedRequests", HsType::list(HsType::Int)),
                        field("rosterRole", HsType::Named("ActorContextRole")),
                        field("rosterBoundWorktree", HsType::maybe(HsType::Text)),
                        field("rosterForkGroup", HsType::maybe(HsType::Int)),
                        field("rosterHaskellScope", HsType::Int),
                        field("rosterProviderThread", HsType::maybe(HsType::Text)),
                        field("rosterProviderParentThread", HsType::maybe(HsType::Text)),
                        field(
                            "rosterFirstUsage",
                            HsType::maybe(HsType::Named("ProviderUsageObservation")),
                        ),
                        field(
                            "rosterLatestUsage",
                            HsType::maybe(HsType::Named("ProviderUsageObservation")),
                        ),
                        field(
                            "rosterUsageSummary",
                            HsType::maybe(HsType::Named("ProviderUsageSummary")),
                        ),
                        field(
                            "rosterLatestTurnUsage",
                            HsType::maybe(HsType::Named("ProviderUsageSummary")),
                        ),
                        field(
                            "rosterCacheBoundary",
                            HsType::maybe(HsType::Named("CacheBoundaryReason")),
                        ),
                        field("rosterEventWatermark", HsType::Int),
                        field(
                            "rosterWorkbenchPosture",
                            HsType::Named("AgentWorkbenchPosture"),
                        ),
                    ],
                },
                json: JsonInstance::None,
                derives: NO_WIRE,
                domain: None,
                doc: &["One authorized actor visible to the executing supervisor."],
            },
            TypeDef {
                name: "AgentForgetOutcome",
                wire_rust: None,
                shape: TypeShape::Sum {
                    variants: vec![
                        variant("AgentForgotten", vec![]),
                        variant("AgentForgetRunning", vec![]),
                        variant(
                            "AgentForgetRetained",
                            vec![
                                HsType::List(Box::new(HsType::Int)),
                                HsType::List(Box::new(HsType::Int)),
                            ],
                        ),
                        variant("AgentForgetUnavailable", vec![]),
                    ],
                },
                json: JsonInstance::None,
                derives: NO_WIRE,
                domain: None,
                doc: &["Explicit, refusal-bearing release of terminal actor observations."],
            },
        ],
        foreign_types: &[
            ("ActorContextRole", "crate::ActorContextRoleWire"),
            (
                "ProviderUsageObservation",
                "crate::ProviderUsageObservationWire",
            ),
            ("ProviderUsageSummary", "crate::ProviderUsageSummaryWire"),
        ],
        errors: None,
        verbs: vec![
            Verb {
                ctor: "AgentInspectCleanupWith",
                method: "agent_inspect_cleanup_with",
                args: vec![Arg {
                    name: "group",
                    ty: HsType::Int,
                    rust: RustBinding::Path("i64"),
                }],
                ret: HsType::Named("CleanupPlan"),
                errors: None,
                handling: HandlingClass::Actor,
                extract: None,
            },
            Verb {
                ctor: "AgentInspectWith",
                method: "agent_inspect_with",
                args: vec![Arg {
                    name: "actor",
                    ty: HsType::Tuple(vec![HsType::Int, HsType::Int]),
                    rust: RustBinding::Path("(i64, i64)"),
                }],
                ret: HsType::maybe(HsType::Named("AgentRosterEntry")),
                errors: None,
                handling: HandlingClass::Actor,
                extract: None,
            },
            Verb {
                ctor: "AgentListWith",
                method: "agent_list_with",
                args: vec![],
                ret: HsType::List(Box::new(HsType::Named("AgentRosterEntry"))),
                errors: None,
                handling: HandlingClass::Actor,
                extract: None,
            },
            Verb {
                ctor: "AgentGroupListWith",
                method: "agent_group_list_with",
                args: vec![Arg {
                    name: "group",
                    ty: HsType::Int,
                    rust: RustBinding::Path("i64"),
                }],
                ret: HsType::maybe(HsType::list(HsType::Named("AgentRosterEntry"))),
                errors: None,
                handling: HandlingClass::Actor,
                extract: None,
            },
            Verb {
                ctor: "AgentForgetWith",
                method: "agent_forget_with",
                args: vec![Arg {
                    name: "actor",
                    ty: HsType::Tuple(vec![HsType::Int, HsType::Int]),
                    rust: RustBinding::Path("(i64, i64)"),
                }],
                ret: HsType::Named("AgentForgetOutcome"),
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

fn field(hs_name: &'static str, ty: HsType) -> RecordField {
    RecordField {
        hs_name,
        rust_name: hs_name,
        ty,
        doc: &[],
    }
}
