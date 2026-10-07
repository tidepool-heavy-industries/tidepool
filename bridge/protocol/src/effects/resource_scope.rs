//! Runtime-owned lexical cleanup scopes. Live body results remain in Haskell.

use crate::hs::HsType;
use crate::schema::{
    Arg, AuthoredSurface, Effect, HandlingClass, JsonInstance, Polymorphism, RustBinding,
    SumVariant, TypeDef, TypeShape, VariantFields, Verb, WireDerive, WireDerives,
};

const DERIVES: WireDerives = WireDerives(&[
    WireDerive::ToHaskell,
    WireDerive::FromHaskell,
    WireDerive::Clone,
    WireDerive::Debug,
    WireDerive::PartialEq,
    WireDerive::Eq,
    WireDerive::Serialize,
    WireDerive::Deserialize,
]);

fn sum(name: &'static str, wire: &'static str, variants: Vec<SumVariant>) -> TypeDef {
    TypeDef {
        name,
        wire_rust: Some(wire),
        haskell_module: Some("Tidepool.Effects.Core"),
        shape: TypeShape::Sum { variants },
        json: JsonInstance::None,
        derives: DERIVES,
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

#[must_use]
pub fn resource_scopes() -> Effect {
    let mut scope = sum(
        "Scope",
        "ResourceScopeId",
        vec![variant("ScopeToken", vec![HsType::Int])],
    );
    scope.derives = WireDerives(&[
        WireDerive::ToHaskell,
        WireDerive::FromHaskell,
        WireDerive::Clone,
        WireDerive::Copy,
        WireDerive::Debug,
        WireDerive::PartialEq,
        WireDerive::Eq,
        WireDerive::Hash,
        WireDerive::Serialize,
        WireDerive::Deserialize,
    ]);
    Effect {
        name: "ResourceScopes",
        authored_surface: AuthoredSurface::OPAQUE,
        handler: "ResourceScopesDecodeHandler",
        handler_module: "resource_scopes",
        req_enum: "ResourceScopesReq",
        decl_fn: "resource_scopes_decl",
        description: &[
            "Use Tidepool.Scope.withScope to bound explicitly scoped resources in reusable Haskell functions. ",
            "The runtime registers cleanup ownership before running the callback and finalizes it on return, failure or cancellation. ",
            "Resources select InScope explicitly; defaults keep their existing ownership. Returning a handle does not transfer lifetime. ",
            "Body and cleanup outcomes remain independent; uncertain external cleanup retains its finalization obligation.",
        ],
        prompt_card: None,
        type_params: &[],
        default_row_args: &[],
        extra_imports: &["import Tidepool.Scope"],
        type_defs: vec![
            scope,
            sum(
                "ScopeFailure",
                "ScopeFailure",
                vec![
                    variant("ScopeRejected", vec![HsType::Text]),
                    variant("ScopeEvaluationFailed", vec![HsType::Text]),
                    variant("ScopeCancelled", vec![]),
                ],
            ),
            sum(
                "CleanupError",
                "CleanupError",
                vec![variant("ScopeCleanupUnconfirmed", vec![HsType::Text])],
            ),
        ],
        external_types: &[crate::schema::ExternalType {
            haskell_name: "RequestSite",
            rust_wire: "i64",
            core_module: Some("Tidepool.Internal.RequestSite"),
        }],
        errors: None,
        verbs: vec![
            Verb {
                ctor: "ScopeRunWith",
                method: "scope_run_with",
                args: vec![Arg {
                    name: "site",
                    ty: HsType::app(
                        HsType::app(HsType::Named("RequestSite"), HsType::TypeList(vec![])),
                        HsType::Tuple(vec![
                            HsType::either(HsType::Named("ScopeFailure"), HsType::Unit),
                            HsType::either(HsType::Named("CleanupError"), HsType::Unit),
                        ]),
                    ),
                    rust: RustBinding::External,
                }, Arg {
                    name: "body",
                    // The callback carries the caller's concrete effects and
                    // a parent-retained exit cell, never a serialized result.
                    ty: HsType::func(
                        HsType::Int,
                        HsType::app(
                            HsType::app(HsType::Named("Eff"), HsType::Var("bodyEffs")),
                            HsType::Unit,
                        ),
                    ),
                    rust: RustBinding::HaskellValue,
                }],
                ret: HsType::Tuple(vec![
                    HsType::either(HsType::Named("ScopeFailure"), HsType::Unit),
                    HsType::either(HsType::Named("CleanupError"), HsType::Unit),
                ]),
                errors: None,
                handling: HandlingClass::Actor,
            },
            Verb {
                ctor: "ScopeDoneWith",
                method: "scope_done_with",
                args: vec![Arg {
                    name: "scope",
                    ty: HsType::Int,
                    rust: RustBinding::Derived,
                }],
                ret: HsType::Unit,
                errors: None,
                handling: HandlingClass::Actor,
            },
        ],
        helpers: vec![],
        polymorphism: Polymorphism::None,
        generated_handler: false,
        handler_execution: crate::schema::HandlerExecution::Immediate,
        caller_principal: false,
    }
}
