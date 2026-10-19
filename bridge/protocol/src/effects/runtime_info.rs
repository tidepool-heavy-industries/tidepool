//! Read-only facts from the executing actor's existing owners.

use crate::hs::HsType;
use crate::schema::ExternalType;
use crate::schema::{
    Effect, ErrorAdt, ErrorField, ErrorVariant, HandlingClass, Helper, HelperBody, JsonInstance,
    Polymorphism, RecordField, RustBinding, SumVariant, TypeDef, TypeShape, Verb, WireDerive,
    WireDerives,
};

const WIRE: WireDerives = WireDerives(&[
    WireDerive::ToHaskell,
    WireDerive::FromHaskell,
    WireDerive::Clone,
    WireDerive::Debug,
    WireDerive::PartialEq,
    WireDerive::Eq,
]);

#[must_use]
pub fn runtime_info() -> Effect {
    Effect {
        name: "RuntimeInfo",
        authored_surface: crate::schema::AuthoredSurface::All,
        handler: "RuntimeInfoDecodeHandler",
        handler_module: "runtime_info",
        req_enum: "RuntimeInfoReq",
        decl_fn: "runtime_info_decl",
        description: &["Read executing installation and configured inference facts, and startup evidence as of this query. Initialization can observe Preparing, never its own future publication. These observations grant no runtime authority."],
        prompt_card: None,
        type_params: &[], default_row_args: &[], extra_imports: &[],
        external_types: &[
            ExternalType { haskell_name: "Model", rust_wire: "Model", core_module: None },
            ExternalType { haskell_name: "Reasoning", rust_wire: "Reasoning", core_module: None },
            ExternalType { haskell_name: "InitialContextObservation", rust_wire: "InitialContextObservation", core_module: None },
        ],
        type_defs: vec![
            record("RuntimeInfoSnapshot", vec![
                field("runtimeExecutingSpec", "executing_spec", HsType::maybe(HsType::Named("RuntimeSpec"))),
                field("runtimeExecutingGeneration", "executing_generation", HsType::maybe(HsType::Int)),
                field("runtimeConfiguredInference", "configured_inference", HsType::Named("RuntimeInference")),
                field("runtimeStartup", "startup", HsType::Named("RuntimeStartup")),
            ], &["Facts are owner-derived at this query. Executing spec/generation can differ from a later active installation; startup is historical lifecycle evidence."]),
            record("RuntimeSpec", vec![
                field("runtimeSpecInstall", "install", HsType::Int),
                field("runtimeSpecEntry", "entry", HsType::maybe(HsType::Text)),
                field("runtimeSpecRevision", "revision", HsType::maybe(HsType::Text)),
                field("runtimeSpecSource", "source", HsType::Text),
                field("runtimeSpecTools", "tools", HsType::list(HsType::Named("RuntimeTool"))),
            ], &["The exact installed implementation retained by this execution, including its declared tool surface."]),
            record("RuntimeTool", vec![
                field("runtimeToolName", "name", HsType::Text),
                field("runtimeToolSchedule", "schedule", HsType::Named("RuntimeToolSchedule")),
                field("runtimeToolEffects", "effects", HsType::list(HsType::Text)),
            ], &[]),
            sum("RuntimeToolSchedule", vec![("RuntimeAsynchronous", vec![]), ("RuntimeBeforeNextInference", vec![])], &[]),
            sum("RuntimeInference", vec![
                ("RuntimeInferenceUnavailable", vec![]),
                ("RuntimeConfiguredInference", vec![HsType::Named("Model"), HsType::Named("Reasoning")]),
            ], &["Configured admitted inference, not the effective Store selection or any pending context edit."]),
            sum("RuntimeStartup", vec![
                ("RuntimeStartupNotAdmitted", vec![]),
                ("RuntimeStartupObserved", vec![HsType::Named("InitialContextObservation")]),
            ], &["Not admitted differs from a spec with no initializer. Preparing differs from retained result and acknowledged context publication."]),
        ],
        errors: Some(ErrorAdt { name: "RuntimeInfoError", variants: vec![ErrorVariant {
            ctor: "RuntimeInfoUnavailable", fields: vec![ErrorField { name: "detail", ty: HsType::Text, rust: RustBinding::Derived }],
            doc: "the executing request or its lifecycle owner cannot be observed",
        }] }),
        verbs: vec![Verb { ctor: "GetRuntimeInfoWith", method: "get_runtime_info", args: vec![],
            ret: HsType::Named("RuntimeInfoSnapshot"), errors: Some("RuntimeInfoError"), handling: HandlingClass::Actor }],
        helpers: vec![Helper { name: "getRuntimeInfo", ctor: Some("GetRuntimeInfoWith"), substrate: false,
            doc: &["Observe this execution's installed spec and configured inference, plus startup evidence as of the query. Safe during initialContext; no filesystem or memory-policy inference."], body: HelperBody::Nullary }],
        polymorphism: Polymorphism::None, generated_handler: false,
        handler_execution: crate::schema::HandlerExecution::Immediate, caller_principal: false,
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
fn record(name: &'static str, fields: Vec<RecordField>, doc: &'static [&'static str]) -> TypeDef {
    TypeDef {
        name,
        wire_rust: Some(name),
        haskell_module: None,
        shape: TypeShape::Record { fields },
        json: JsonInstance::None,
        derives: WIRE,
        domain: None,
        doc,
    }
}
fn sum(
    name: &'static str,
    variants: Vec<(&'static str, Vec<HsType>)>,
    doc: &'static [&'static str],
) -> TypeDef {
    TypeDef {
        name,
        wire_rust: Some(name),
        haskell_module: None,
        shape: TypeShape::Sum {
            variants: variants
                .into_iter()
                .map(|(ctor, fields)| SumVariant {
                    ctor,
                    fields: crate::types::VariantFields::Positional(fields),
                    doc: &[],
                })
                .collect(),
        },
        json: JsonInstance::None,
        derives: WIRE,
        domain: None,
        doc,
    }
}
