//! Fresh-context actor launch capability for the interactive facade.

use crate::hs::HsType;
use crate::schema::{Arg, Effect, HandlingClass, Polymorphism, RustBinding, Verb};

/// Launch a new persistent actor without inheriting the caller's provider context.
#[must_use]
pub fn agent_launch() -> Effect {
    Effect {
        name: "AgentLaunch",
        authored_surface: crate::schema::AuthoredSurface::OPAQUE,
        handler: "AgentLaunchDecodeHandler",
        handler_module: "agent_launch",
        req_enum: "AgentLaunchReq",
        decl_fn: "agent_launch_decl",
        description: &["Private fresh-agent launch substrate used by the Shoal facade."],
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
        verbs: vec![Verb {
            ctor: "AgentLaunchWith",
            method: "agent_launch_with",
            args: launch_args(false),
            ret: launched_actor_type(),
            errors: None,
            handling: HandlingClass::Actor,
            extract: None,
        }],
        helpers: Vec::new(),
        polymorphism: Polymorphism::None,
        dispatched: false,
    }
}

pub(crate) fn launch_args(forked: bool) -> Vec<Arg> {
    let mut args = vec![
        Arg {
            name: "label",
            ty: HsType::Text,
            rust: RustBinding::Derived,
        },
        Arg {
            name: "entry",
            ty: HsType::func(
                HsType::Int,
                HsType::app(
                    HsType::app(HsType::Named("Eff"), HsType::Var("childEffs")),
                    HsType::Unit,
                ),
            ),
            rust: RustBinding::CoreValue,
        },
    ];
    if forked {
        args.push(Arg {
            name: "forkGroup",
            ty: HsType::Int,
            rust: RustBinding::Derived,
        });
    }
    args.extend([
        Arg {
            name: "role",
            ty: HsType::Named("ActorLaunchRole"),
            rust: RustBinding::Path("crate::ActorLaunchRoleWire"),
        },
        Arg {
            name: "profile",
            ty: HsType::Named("ActorEffectProfile"),
            rust: RustBinding::Path("crate::ActorEffectProfileWire"),
        },
        Arg {
            name: "launchWorktrees",
            ty: HsType::List(Box::new(HsType::Text)),
            rust: RustBinding::Derived,
        },
    ]);
    args
}

pub(crate) fn launched_actor_type() -> HsType {
    HsType::Tuple(vec![HsType::Int, HsType::Int, HsType::Text])
}
