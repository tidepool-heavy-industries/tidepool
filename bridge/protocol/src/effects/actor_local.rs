//! The actor-local lifecycle and mailbox suspension boundary.
//!
//! Unlike the outward `Actor` capability, this effect is indexed by the
//! current actor's protocol. The enclosing definition fixes successful exit.
//! Its raw constructors are kernel substrate; `Tidepool.Actor` exposes the
//! authored operations.

use crate::hs::HsType;
use crate::schema::{
    Arg, Effect, HandlingClass, JsonInstance, Polymorphism, RustBinding, SumVariant, TypeDef,
    TypeParam, TypeShape, VariantFields, Verb, WireDerive, WireDerives,
};

const TYPE_PARAMS: &[TypeParam] = &[TypeParam::unary("api")];

/// The indexed actor-local effect.
#[must_use]
pub fn actor_local() -> Effect {
    Effect {
        name: "ActorLocal",
        authored_surface: crate::schema::AuthoredSurface::OPAQUE,
        handler: "ActorLocalDecodeHandler",
        handler_module: "actor_local",
        req_enum: "ActorLocalReq",
        decl_fn: "actor_local_decl",
        description: &[
            "Private actor-local lifecycle and mailbox substrate. Authored code uses ",
            "`Tidepool.Actor`; the protocol index ties requests to the installed program. ",
            "The enclosing ActorDefinition separately fixes successful exit.",
        ],
        prompt_card: None,
        type_params: TYPE_PARAMS,
        default_row_args: &["Maybe"],
        helpers_row_polymorphic: true,
        extra_imports: &["import Tidepool.Actor"],
        type_defs: vec![TypeDef {
            name: "ActorInputOrigin",
            wire_rust: None,
            haskell_module: None,
            shape: TypeShape::Sum { variants: vec![
                SumVariant { ctor: "ActorStartup", fields: VariantFields::Positional(vec![]), doc: &[] },
                SumVariant { ctor: "ActorMessageFrom", fields: VariantFields::Positional(vec![HsType::Tuple(vec![HsType::Int, HsType::Int])]), doc: &[] },
                SumVariant { ctor: "ActorProgressFrom", fields: VariantFields::Positional(vec![HsType::Int]), doc: &[] },
                SumVariant { ctor: "ActorSettlementFrom", fields: VariantFields::Positional(vec![HsType::Int]), doc: &[] },
                SumVariant { ctor: "ActorCommandFrom", fields: VariantFields::Positional(vec![HsType::Text]), doc: &[] },
                SumVariant { ctor: "ActorLifecycleFrom", fields: VariantFields::Positional(vec![HsType::Tuple(vec![HsType::Int, HsType::Int])]), doc: &[] },
            ] },
            json: JsonInstance::None,
            derives: WireDerives(&[WireDerive::Debug, WireDerive::PartialEq, WireDerive::Eq]),
            domain: None,
            doc: &["Runtime identity of the currently handled input; it conveys no resource authority."],
        }],
        foreign_types: &[],
        errors: None,
        verbs: vec![
            Verb {
                ctor: "ActorLocalContextWith",
                method: "actor_local_context_with",
                args: vec![],
                ret: HsType::Tuple(vec![HsType::Tuple(vec![HsType::Int, HsType::Int]), HsType::Named("ActorInputOrigin")]),
                errors: None,
                handling: HandlingClass::Actor,
                extract: None,
            },
            source_attach("ActorLocalAttachProgressSourceWith", "actor_local_attach_progress_source_with", Arg {
                name: "request", ty: HsType::Int, rust: RustBinding::Derived,
            }),
            source_attach("ActorLocalAttachSettlementSourceWith", "actor_local_attach_settlement_source_with", Arg {
                name: "request", ty: HsType::Int, rust: RustBinding::Derived,
            }),
            source_attach("ActorLocalAttachCommandSourceWith", "actor_local_attach_command_source_with", Arg {
                name: "job", ty: HsType::Text, rust: RustBinding::Path("String"),
            }),
            source_attach("ActorLocalAttachLifecycleSourceWith", "actor_local_attach_lifecycle_source_with", Arg {
                name: "target", ty: HsType::Tuple(vec![HsType::Int, HsType::Int]), rust: RustBinding::Path("(i64, i64)"),
            }),
            Verb {
                ctor: "ActorReceiveWith",
                method: "actor_receive_with",
                args: vec![
                    Arg {
                        name: "site",
                        ty: HsType::Int,
                        rust: RustBinding::Derived,
                    },
                    Arg {
                        name: "handler",
                        ty: HsType::forall(
                            vec!["result"],
                            HsType::func(
                                HsType::app(HsType::Var("api"), HsType::Var("result")),
                                HsType::app(
                                    HsType::app(HsType::Named("Eff"), HsType::Var("handlerEffs")),
                                    HsType::Unit,
                                ),
                            ),
                        ),
                        rust: RustBinding::HaskellValue,
                    },
                ],
                ret: HsType::Var("next"),
                errors: None,
                handling: HandlingClass::Actor,
                extract: None,
            },
            Verb {
                ctor: "ActorCheckpointWith",
                method: "actor_checkpoint_with",
                args: vec![
                    Arg {
                        name: "site",
                        ty: HsType::Int,
                        rust: RustBinding::Derived,
                    },
                    Arg {
                        name: "state",
                        ty: HsType::Var("state"),
                        rust: RustBinding::HaskellValue,
                    },
                ],
                ret: HsType::Unit,
                errors: None,
                handling: HandlingClass::Actor,
                extract: None,
            },
        ],
        helpers: Vec::new(),
        polymorphism: Polymorphism::None,
        generated_handler: false,
        caller_principal: false,
    }
}

fn source_attach(ctor: &'static str, method: &'static str, target: Arg) -> Verb {
    Verb {
        ctor,
        method,
        args: vec![
            Arg {
                name: "owner",
                ty: HsType::Tuple(vec![HsType::Int, HsType::Int]),
                rust: RustBinding::Path("(i64, i64)"),
            },
            target,
            Arg {
                name: "entry",
                ty: HsType::func(
                    HsType::Int,
                    HsType::app(
                        HsType::app(HsType::Named("Eff"), HsType::Var("sourceEffs")),
                        HsType::Unit,
                    ),
                ),
                rust: RustBinding::HaskellValue,
            },
        ],
        ret: HsType::Either(Box::new(HsType::Text), Box::new(HsType::Unit)),
        errors: None,
        handling: HandlingClass::Actor,
        extract: None,
    }
}
