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
        description: &[
            "Run a bounded model subroutine with an explicit tool record and typed or textual result through `Tidepool.Model`. ",
            "Use it when a local reasoning step needs its own instructions and tool loop; use Jev for a finite semantic judgment over supplied evidence, and a child agent when work needs independent context, checkout or lifecycle. ",
            "Haskell constructs the tool capabilities, input and result decoder, then pattern matches the outcome to continue. ",
            "Callbacks execute in the caller's effect row; every invocation shares the admitted cell's model budget. ",
            "A decoded result establishes its schema, not the truth of its claims: retain evidence and validate task-specific invariants before acting.",
        ],
        prompt_card: None,
        type_params: &[],
        default_row_args: &[],
        extra_imports: &["import Tidepool.Model"],
        type_defs: vec![],
        external_types: &[],
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
