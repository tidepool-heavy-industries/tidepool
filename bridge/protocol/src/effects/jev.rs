//! One Jev (TypeSafe System One) judgment request, answered by the host.
//! The request and response bodies cross as JSON text; `Jev.Operators`
//! builds and decodes them.
use crate::hs::HsType;
use crate::schema::{
    Arg, AuthoredSurface, Effect, HandlingClass, JsonInstance, Polymorphism, RustBinding,
    SumVariant, TypeDef, TypeShape, VariantFields, Verb, WireDerive, WireDerives,
};

const CALL_ERROR_DERIVES: WireDerives = WireDerives(&[
    WireDerive::ToHaskell,
    WireDerive::Clone,
    WireDerive::Debug,
    WireDerive::PartialEq,
    WireDerive::Eq,
]);

fn variant(ctor: &'static str, fields: Vec<HsType>) -> SumVariant {
    SumVariant {
        ctor,
        fields: VariantFields::Positional(fields),
        doc: &[],
    }
}

/// The resident Jev effect.
#[must_use]
pub fn jev() -> Effect {
    Effect {
        name: "Jev",
        authored_surface: AuthoredSurface::OPAQUE,
        handler: "JevDecodeHandler",
        handler_module: "jev",
        req_enum: "JevReq",
        decl_fn: "jev_decl",
        description: &[
            "Compose semantic judgment with ordinary Haskell: Jev choices can carry domain values, closures or effectful continuations into the next stage of a program. ",
            "Use `Jev.Operators` (`exomonad-jev` skill), not the private JSON request constructor. ",
            "`J.state` supplies the evidence; `J.ask` batches questions that can be answered from that same packet. ",
            "Choose `J.choice` for one winner, `J.each` with `J.noul` for independently qualifying items, and `J.score` for an ordered rubric. ",
            "`J.handle` dispatches a choice to handlers receiving its original typed payload; `J.settle` adds a policy-controlled doubt branch. Build semantic predicates, selectors and interpreters from these parts. ",
            "Inside a record actor, combine its state and incoming event with a judgment, then run the selected continuation: update state, gather evidence or commission typed agent work. ",
            "Use deterministic code for exact checks and known transitions; use Jev where meaning changes the next action. ",
            "Distinguish call failure, doubt, and an explicit insufficient-evidence alternative; each can select its own recovery, next read or handback. ",
            "Retain packets, decisions and independently checked outcomes when comparing policy behavior; confidence is not a substitute for outcome calibration.",
        ],
        prompt_card: None,
        type_params: &[],
        default_row_args: &[],
        extra_imports: &[],
        type_defs: vec![TypeDef {
            name: "JevCallError",
            wire_rust: Some("JevCallFailure"),
            haskell_module: Some("Tidepool.Effects.Core"),
            shape: TypeShape::Sum {
                variants: vec![
                    variant("JevUnconfigured", vec![]),
                    variant("JevCallCap", vec![]),
                    variant("JevTransport", vec![HsType::Text]),
                    variant("JevTimeout", vec![]),
                    variant("JevHttp", vec![HsType::Int, HsType::Text]),
                    variant("JevCircuitOpen", vec![HsType::Int, HsType::Int]),
                    variant("JevClientSetup", vec![HsType::Text]),
                    variant("JevBodyLimit", vec![]),
                    variant("JevMalformed", vec![HsType::Text]),
                ],
            },
            json: JsonInstance::None,
            derives: CALL_ERROR_DERIVES,
            domain: None,
            doc: &[],
        }],
        external_types: &[],
        errors: None,
        verbs: vec![Verb {
            ctor: "JevAskWith",
            method: "jev_ask_with",
            args: vec![Arg {
                name: "request",
                ty: HsType::Text,
                rust: RustBinding::Derived,
            }],
            ret: HsType::either(HsType::Named("JevCallError"), HsType::Text),
            errors: None,
            handling: HandlingClass::Actor,
        }],
        helpers: vec![],
        polymorphism: Polymorphism::None,
        generated_handler: false,
        handler_execution: crate::schema::HandlerExecution::Immediate,
        caller_principal: false,
    }
}
