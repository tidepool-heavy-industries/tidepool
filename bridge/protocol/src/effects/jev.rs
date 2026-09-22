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
            "Send one Jev judgment request (JSON text) to TypeSafe's System One and ",
            "return the response body (JSON text) or a typed call failure.",
        ],
        prompt_card: None,
        type_params: &[],
        default_row_args: &[],
        helpers_row_polymorphic: true,
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
        foreign_types: &[],
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
            extract: None,
        }],
        helpers: vec![],
        polymorphism: Polymorphism::None,
        dispatched: false,
        caller_principal: false,
    }
}
