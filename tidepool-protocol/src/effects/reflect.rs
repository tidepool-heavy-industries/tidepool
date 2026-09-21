//! The calling actor's own completed conversation turns.
//!
//! Only the caller's conversation. The verb takes no actor, thread, or path
//! argument, so there is nothing to widen: an actor can read what it said and
//! was told, and cannot reach another actor's history or an arbitrary file.
//!
//! The wire types are generated into `tidepool-bridge-effects`, so the Rust
//! struct the actor kernel fills and the Haskell record an author reads are
//! one declaration.

use crate::hs::HsType;
use crate::schema::{
    Arg, Effect, HandlingClass, Helper, HelperBody, JsonInstance, Polymorphism, RecordField,
    RustBinding, SumVariant, TypeDef, TypeShape, VariantFields, Verb, WireDerive, WireDerives,
};

const WIRE: WireDerives = WireDerives(&[
    WireDerive::ToHaskell,
    WireDerive::Clone,
    WireDerive::Debug,
    WireDerive::PartialEq,
    WireDerive::Eq,
]);

/// The Reflect effect, completely.
#[must_use]
pub fn reflect() -> Effect {
    Effect {
        name: "Reflect",
        authored_surface: crate::schema::AuthoredSurface::All,
        handler: "ReflectDecodeHandler",
        handler_module: "reflect",
        req_enum: "ReflectReq",
        decl_fn: "reflect_decl",
        description: &[
            "Read your own recent conversation as data. `reflect n` returns your last ",
            "n COMPLETED turns, oldest first, each carrying the messages, tool calls ",
            "and tool results that belonged to it. The turn you are executing has not ",
            "completed and is never included; fewer than n completed turns returns the ",
            "ones that exist and `n <= 0` returns none. It reads only the caller's own ",
            "conversation — there is no argument naming another actor or a file. ",
            "`Left ReflectUnbound` means this context has no bound conversation to ",
            "read, which an operator proxy and a recreated host both are; the root's ",
            "conversation is never substituted for a caller's. Because it is an ",
            "ordinary effect, an authored function can fetch its own recent context ",
            "once and reuse it across a whole sequence of later calls.",
        ],
        prompt_card: None,
        type_params: &[],
        default_row_args: &[],
        helpers_row_polymorphic: true,
        extra_imports: &[],
        type_defs: vec![
            TypeDef {
                name: "ConversationRole",
                wire_rust: Some("RfRole"),
                haskell_module: None,
                shape: TypeShape::Sum {
                    variants: ["RoleSystem", "RoleDeveloper", "RoleUser", "RoleAssistant"]
                        .into_iter()
                        .map(|ctor| SumVariant {
                            ctor,
                            fields: VariantFields::Positional(Vec::new()),
                            doc: &[],
                        })
                        .collect(),
                },
                json: JsonInstance::None,
                derives: WIRE,
                domain: None,
                doc: &["Who authored a message, as the provider recorded it."],
            },
            TypeDef {
                name: "TurnItem",
                wire_rust: Some("RfTurnItem"),
                haskell_module: None,
                shape: TypeShape::Sum {
                    variants: vec![
                        SumVariant {
                            ctor: "TurnMessage",
                            fields: positional_fields![
                                HsType::Named("ConversationRole"),
                                HsType::Text,
                            ],
                            doc: &["One message and its author."],
                        },
                        SumVariant {
                            ctor: "TurnToolCall",
                            fields: positional_fields![HsType::Text, HsType::Text, HsType::Text],
                            doc: &[
                                "Call identity, tool name, and the arguments as the provider",
                                "recorded them. The arguments are the provider's own JSON text.",
                            ],
                        },
                        SumVariant {
                            ctor: "TurnToolResult",
                            fields: positional_fields![HsType::Text, HsType::Text],
                            doc: &[
                                "The call identity its `TurnToolCall` carries, and the output",
                                "that call produced.",
                            ],
                        },
                    ],
                },
                json: JsonInstance::None,
                derives: WIRE,
                domain: None,
                doc: &[
                    "One item inside a turn. A call and its result are separate items",
                    "sharing one call identity, so a reader can rejoin the pair.",
                ],
            },
            TypeDef {
                name: "ConversationTurn",
                wire_rust: Some("RfConversationTurn"),
                haskell_module: None,
                shape: TypeShape::Record {
                    fields: vec![
                        field("turnIdentity", "identity", HsType::Text),
                        field("turnStartedAt", "started_at", HsType::maybe(HsType::Text)),
                        field(
                            "turnCompletedAt",
                            "completed_at",
                            HsType::maybe(HsType::Text),
                        ),
                        field(
                            "turnItems",
                            "items",
                            HsType::list(HsType::Named("TurnItem")),
                        ),
                    ],
                },
                json: JsonInstance::None,
                derives: WIRE,
                domain: None,
                doc: &[
                    "One completed turn: everything that happened between one request",
                    "and the answer to it, in provider order. Absent timestamps mean the",
                    "record carried none, not an instant zero.",
                ],
            },
            TypeDef {
                name: "ReflectError",
                wire_rust: Some("RfError"),
                haskell_module: None,
                shape: TypeShape::Sum {
                    variants: vec![
                        SumVariant {
                            ctor: "ReflectUnbound",
                            fields: VariantFields::Positional(Vec::new()),
                            doc: &[
                                "This context has no bound conversation to read. Another",
                                "conversation is never read in its place.",
                            ],
                        },
                        SumVariant {
                            ctor: "ReflectUnreadable",
                            fields: positional_fields![HsType::Text],
                            doc: &["The bound conversation exists but could not be read."],
                        },
                    ],
                },
                json: JsonInstance::None,
                derives: WIRE,
                domain: None,
                doc: &["Why a caller's own conversation could not be returned."],
            },
        ],
        foreign_types: &[],
        errors: None,
        verbs: vec![Verb {
            ctor: "ReflectWith",
            method: "reflect_with",
            args: vec![Arg {
                name: "count",
                ty: HsType::Int,
                rust: RustBinding::Path("i64"),
            }],
            ret: HsType::Either(
                Box::new(HsType::Named("ReflectError")),
                Box::new(HsType::list(HsType::Named("ConversationTurn"))),
            ),
            errors: None,
            handling: HandlingClass::Actor,
            extract: None,
        }],
        helpers: vec![Helper {
            name: "reflect",
            ctor: Some("ReflectWith"),
            substrate: false,
            doc: &[
                "`reflect n` reads your OWN last n completed conversation turns, oldest",
                "first, each carrying its messages, tool calls and tool results. The turn",
                "you are executing is not complete and is never among them; fewer than n",
                "completed turns returns the ones that exist, and `n <= 0` returns none.",
                "Natural spelling: `Right recent <- reflect 5`. `Left ReflectUnbound`",
                "means this context has no conversation of its own — no other actor's is",
                "returned in its place. Bind the result once and reuse it across the",
                "calls that need it rather than reading it again per call.",
            ],
            body: HelperBody::Pointfree,
        }],
        polymorphism: Polymorphism::None,
        dispatched: false,
        caller_principal: false,
    }
}

fn field(hs_name: &'static str, rust_name: &'static str, ty: HsType) -> RecordField {
    RecordField {
        hs_name,
        rust_name,
        ty,
        doc: &[],
    }
}
