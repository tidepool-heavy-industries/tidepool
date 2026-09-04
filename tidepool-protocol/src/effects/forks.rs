//! Cache-preserving actor-fork admission capability.

use crate::hs::HsType;
use crate::schema::{Arg, Effect, HandlingClass, Polymorphism, RustBinding, Verb};

use super::agent_launch::{launch_args, launched_actor_type};

#[must_use]
pub fn forks() -> Effect {
    Effect {
        name: "Forks",
        authored_surface: crate::schema::AuthoredSurface::OPAQUE,
        handler: "ForksDecodeHandler",
        handler_module: "forks",
        req_enum: "ForksReq",
        decl_fn: "forks_decl",
        description: &["Private atomic context-fork admission substrate used by `unfold`."],
        prompt_card: None,
        type_params: &[],
        default_row_args: &[],
        helpers_row_polymorphic: true,
        extra_imports: &[],
        type_defs: Vec::new(),
        foreign_types: &[
            ("ActorLaunchRole", "crate::ActorLaunchRoleWire"),
            ("ActorEffectProfile", "crate::ActorEffectProfileWire"),
        ],
        errors: None,
        verbs: vec![
            Verb {
                ctor: "ForksBeginWith",
                method: "forks_begin_with",
                args: vec![
                    Arg {
                        name: "relative",
                        ty: HsType::Bool,
                        rust: RustBinding::Derived,
                    },
                    Arg {
                        name: "group",
                        ty: HsType::Text,
                        rust: RustBinding::Derived,
                    },
                    Arg {
                        name: "branches",
                        ty: HsType::List(Box::new(HsType::Text)),
                        rust: RustBinding::Derived,
                    },
                ],
                ret: HsType::Tuple(vec![
                    HsType::Int,
                    HsType::Text,
                    HsType::List(Box::new(HsType::Text)),
                ]),
                errors: None,
                handling: HandlingClass::Actor,
                extract: None,
            },
            Verb {
                ctor: "ForksStartWith",
                method: "forks_start_with",
                args: launch_args(true),
                ret: launched_actor_type(),
                errors: None,
                handling: HandlingClass::Actor,
                extract: None,
            },
            group_verb("ForksCommitWith", "forks_commit_with"),
            group_verb("ForksAbortWith", "forks_abort_with"),
        ],
        helpers: Vec::new(),
        polymorphism: Polymorphism::None,
        dispatched: false,
    }
}

fn group_verb(ctor: &'static str, method: &'static str) -> Verb {
    Verb {
        ctor,
        method,
        args: vec![Arg {
            name: "forkGroup",
            ty: HsType::Int,
            rust: RustBinding::Derived,
        }],
        ret: HsType::Unit,
        errors: None,
        handling: HandlingClass::Actor,
        extract: None,
    }
}
