//! A bounded model turn whose declared callbacks execute in the caller's effects.
use crate::hs::HsType;
use crate::schema::{
    Arg, AuthoredSurface, Effect, ErrorAdt, ErrorField, ErrorVariant, HandlingClass, OuterEffect,
    Polymorphism, RustBinding, Verb,
};

#[must_use]
pub fn model() -> Effect {
    Effect {
        name: "ModelCall",
        authored_surface: AuthoredSurface::OPAQUE,
        handler: "ModelHandler",
        handler_module: "model",
        req_enum: "ModelReq",
        decl_fn: "model_call_decl",
        description: &["Run one bounded model turn with explicitly supplied tools. Callbacks execute in the caller's effect row; every invocation shares the admitted cell's model budget."],
        prompt_card: None,
        type_params: &[],
        default_row_args: &[],
        helpers_row_polymorphic: true,
        extra_imports: &["import Tidepool.Model"],
        type_defs: vec![],
        foreign_types: &[],
        errors: Some(ErrorAdt {
            name: "ModelBoundaryError",
            variants: [
                ("ModelUnavailable", "this cell has no model invocation service"),
                ("ModelRejected", "the invocation or continuation was not admitted"),
                ("ModelTransportFailed", "the invocation service could not return its next step"),
            ].into_iter().map(|(ctor, doc)| ErrorVariant {
                ctor, doc, fields: vec![ErrorField { name: "detail", ty: HsType::Text, rust: RustBinding::Derived }],
            }).collect(),
        }),
        verbs: vec![
            verb("ModelStartWith", "model_start", vec![json("request")], HsType::Value),
            verb("ModelResumeWith", "model_resume", vec![text("invocation"), text("callId"), json("answer")], HsType::Value),
            verb("ModelAnnotateWith", "model_annotate", vec![text("invocation"), text("operation"), json("annotation")], HsType::Value),
            verb("ModelCloseWith", "model_close", vec![text("invocation")], HsType::Unit),
        ],
        helpers: vec![],
        polymorphism: Polymorphism::None,
        generated_handler: true,
        handler_execution: crate::schema::HandlerExecution::BlockingPrepared,
        caller_principal: true,
    }
}
fn text(name: &'static str) -> Arg {
    Arg {
        name,
        ty: HsType::Text,
        rust: RustBinding::Derived,
    }
}
fn json(name: &'static str) -> Arg {
    Arg {
        name,
        ty: HsType::Value,
        rust: RustBinding::JsonValue,
    }
}
fn verb(ctor: &'static str, method: &'static str, args: Vec<Arg>, ret: HsType) -> Verb {
    Verb {
        ctor,
        method,
        args,
        ret,
        errors: Some("ModelBoundaryError"),
        handling: HandlingClass::OuterDispatch(OuterEffect::Model),
    }
}
