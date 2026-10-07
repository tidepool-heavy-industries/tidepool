//! Mounted human forms. Haskell retains decoding and original payloads.
use crate::hs::HsType;
use crate::schema::{
    Arg, Effect, HandlingClass, Helper, HelperBody, JsonInstance, Polymorphism, RustBinding,
    SumVariant, TypeDef, TypeShape, VariantFields, Verb, WireDerive, WireDerives,
};
fn sum(name: &'static str, variants: Vec<SumVariant>) -> TypeDef {
    TypeDef {
        name,
        wire_rust: Some(name),
        haskell_module: Some("Tidepool.Effects.Core"),
        shape: TypeShape::Sum { variants },
        json: JsonInstance::None,
        derives: WireDerives(&[
            WireDerive::ToHaskell,
            WireDerive::FromHaskell,
            WireDerive::Clone,
            WireDerive::Debug,
            WireDerive::PartialEq,
        ]),
        domain: None,
        doc: &[],
    }
}
fn variant(ctor: &'static str, fields: Vec<HsType>) -> SumVariant {
    SumVariant {
        ctor,
        fields: VariantFields::Positional(fields),
        doc: &[],
    }
}
fn arg(name: &'static str, ty: HsType) -> Arg {
    let rust = match ty {
        HsType::Named("FormLease") => RustBinding::Path("tidepool_bridge_effects::FormLease"),
        HsType::Named("FormAttemptId") => {
            RustBinding::Path("tidepool_bridge_effects::FormAttemptId")
        }
        _ => RustBinding::Derived,
    };
    Arg { name, ty, rust }
}
fn json(name: &'static str) -> Arg {
    Arg {
        name,
        ty: HsType::Value,
        rust: RustBinding::JsonValue,
    }
}
#[must_use]
pub fn ask_user() -> Effect {
    let mut defs = vec![
        sum(
            "FormLease",
            vec![variant("FormLeaseToken", vec![HsType::Text])],
        ),
        sum(
            "FormAttemptId",
            vec![variant("FormAttemptToken", vec![HsType::Text])],
        ),
        sum(
            "FormCause",
            vec![
                variant("FormNotInstalled", vec![]),
                variant("FormInterrupted", vec![]),
                variant("FormTransportFailed", vec![HsType::Text]),
                variant("FormMalformed", vec![HsType::Text]),
                variant("FormUnauthorized", vec![]),
                variant("FormClosed", vec![]),
                variant("FormCleanupUnconfirmed", vec![HsType::Text]),
            ],
        ),
        sum(
            "FormAttempt",
            vec![
                variant(
                    "FormSubmitted",
                    vec![HsType::Named("FormAttemptId"), HsType::Value],
                ),
                variant("FormDismissed", vec![]),
            ],
        ),
        sum(
            "FormTransition",
            vec![variant("FormApplied", vec![]), variant("FormStale", vec![])],
        ),
    ];
    defs[3].derives = WireDerives(&[
        WireDerive::ToHaskell,
        WireDerive::Clone,
        WireDerive::Debug,
        WireDerive::PartialEq,
    ]);
    let operations = vec![
        (
            "FormOpenWith",
            "form_open_with",
            "formOpenRaw",
            vec![json("descriptor")],
            HsType::Named("FormLease"),
            &["descriptor"][..],
        ),
        (
            "FormAwaitWith",
            "form_await_with",
            "formAwaitRaw",
            vec![arg("lease", HsType::Named("FormLease"))],
            HsType::Named("FormAttempt"),
            &["lease"][..],
        ),
        (
            "FormRejectWith",
            "form_reject_with",
            "formRejectRaw",
            vec![
                arg("lease", HsType::Named("FormLease")),
                arg("attempt", HsType::Named("FormAttemptId")),
                json("errors"),
            ],
            HsType::Named("FormTransition"),
            &["lease", "attempt", "errors"][..],
        ),
        (
            "FormCommitWith",
            "form_commit_with",
            "formCommitRaw",
            vec![
                arg("lease", HsType::Named("FormLease")),
                arg("attempt", HsType::Named("FormAttemptId")),
                json("presentation"),
            ],
            HsType::Named("FormTransition"),
            &["lease", "attempt", "presentation"][..],
        ),
        (
            "FormCloseWith",
            "form_close_with",
            "formCloseRaw",
            vec![arg("lease", HsType::Named("FormLease"))],
            HsType::Unit,
            &["lease"][..],
        ),
    ];
    Effect {
        name: "AskUser",
        authored_surface: crate::schema::AuthoredSurface::OPAQUE,
        handler: "AskUserHandler",
        handler_module: "ask_user",
        req_enum: "AskUserReq",
        decl_fn: "askuser_decl",
        prompt_card: None,
        description: &[
            "Present an applicative Tidepool.Form to the human operator. One continuation owns one mounted form through validation retries; final answer publication precedes returning Submitted. Dismissal and infrastructure closure remain typed outcomes.",
        ],
        type_params: &[],
        default_row_args: &[],
        extra_imports: &["import Tidepool.Form"],
        type_defs: defs,
        external_types: &[],
        errors: None,
        verbs: operations
            .iter()
            .map(|(ctor, method, _, args, ret, _)| Verb {
                ctor,
                method,
                args: args.clone(),
                ret: HsType::either(HsType::Named("FormCause"), ret.clone()),
                errors: None,
                handling: HandlingClass::AskUserForm,
            })
            .collect(),
        helpers: operations
            .into_iter()
            .map(|(ctor, _, name, _, _, arguments)| Helper {
                name,
                ctor: Some(ctor),
                substrate: true,
                doc: &[],
                body: HelperBody::Applied(arguments),
            })
            .collect(),
        polymorphism: Polymorphism::None,
        generated_handler: false,
        handler_execution: crate::schema::HandlerExecution::Immediate,
        caller_principal: false,
    }
}
