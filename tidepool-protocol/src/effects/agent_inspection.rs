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
                name: "AgentRosterEntry",
                wire_rust: None,
                shape: TypeShape::Record {
                    fields: vec![
                        field("rosterActorId", HsType::Int),
                        field("rosterActorIncarnation", HsType::Int),
                        field("rosterLabel", HsType::Text),
                        field("rosterState", HsType::Named("AgentRosterState")),
                        field("rosterRole", HsType::Named("ActorContextRole")),
                        field("rosterBoundWorktree", HsType::maybe(HsType::Text)),
                        field("rosterForkGroup", HsType::maybe(HsType::Int)),
                        field("rosterHaskellSnapshot", HsType::Int),
                        field("rosterProviderThread", HsType::maybe(HsType::Text)),
                        field("rosterProviderParentThread", HsType::maybe(HsType::Text)),
                        field("rosterCachedInputTokens", HsType::maybe(HsType::Int)),
                        field("rosterUncachedInputTokens", HsType::maybe(HsType::Int)),
                    ],
                },
                json: JsonInstance::None,
                derives: NO_WIRE,
                domain: None,
                doc: &["One authorized actor visible to the executing supervisor."],
            },
        ],
        foreign_types: &[
            ("ActorTerminalStatus", "crate::ActorTerminalStatusWire"),
            ("ActorContextRole", "crate::ActorContextRoleWire"),
        ],
        errors: None,
        verbs: vec![
            Verb {
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
