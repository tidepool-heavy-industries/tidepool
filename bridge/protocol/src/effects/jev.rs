//! One Jev (TypeSafe System One) judgment request, answered by the host.
//! The request and response bodies cross as JSON text; `Jev.Operators`
//! builds and decodes them.
use crate::hs::HsType;
use crate::schema::{
    Arg, AuthoredSurface, Effect, HandlingClass, JsonInstance, Polymorphism, RustBinding,
    SumVariant, TypeDef, TypeShape, VariantFields, Verb, WireDerives,
};

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
            "Semantic judgment inside a typed Haskell program: classify evidence, select a candidate, or choose a prepared continuation with Jev. ",
            "Use `Jev.Operators` (`exomonad-jev` skill), not the private JSON request constructor. ",
            "`J.state` supplies the evidence; `J.ask` batches questions that can be answered from that same packet. ",
            "Choose `J.choice` for one winner, `J.each` with `J.noul` for independently qualifying items, and `J.score` for an ordered rubric. ",
            "Alternatives carry typed payloads, including effectful continuations, so a semantic choice can drive ordinary Haskell control flow without interpreting a rendered label. ",
            "Use deterministic code for exact checks and known transitions; use Jev where meaning changes the next action. ",
            "A policy threshold cannot recover missing evidence or establish authority. Distinguish call failure, doubt, and an explicit insufficient-evidence alternative; fetch discriminating evidence or hand back unresolved cases before acting. ",
            "Retain packets, decisions and independently checked outcomes when comparing policy behavior; confidence is not a substitute for outcome calibration.",
        ],
        prompt_card: None,
        type_params: &[],
        default_row_args: &[],
        extra_imports: &[],
        type_defs: vec![TypeDef {
            name: "JevCallError",
            wire_rust: None,
            haskell_module: None,
            shape: TypeShape::Sum {
                variants: vec![
                    variant("JevUnconfigured", vec![]),
                    variant("JevCallCap", vec![]),
                    variant("JevTransport", vec![HsType::Text]),
                    variant("JevTimeout", vec![]),
                    variant("JevHttp", vec![HsType::Int, HsType::Text]),
                    variant("JevBodyLimit", vec![]),
                    variant("JevMalformed", vec![HsType::Text]),
                ],
            },
            json: JsonInstance::None,
            derives: WireDerives(&[]),
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
