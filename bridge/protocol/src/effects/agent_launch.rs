//! Fresh-context actor launch capability for the interactive facade.

use crate::hs::HsType;
use crate::schema::{Arg, Effect, HandlingClass, JsonInstance, Polymorphism, RustBinding, SumVariant, TypeDef, TypeShape, VariantFields, Verb, WireDerives};

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
        description: &["Private fresh-agent launch substrate used by the Exomonad facade."],
        prompt_card: None,
        type_params: &[],
        default_row_args: &[],
        extra_imports: &[],
        type_defs: spawn_types(),
        external_types: &[
            crate::schema::ExternalType {
                haskell_name: "ActorLaunchRole",
                rust_wire: "crate::ActorLaunchRoleWire",
                core_module: None,
            },
            crate::schema::ExternalType {
                haskell_name: "ActorEffectProfile",
                rust_wire: "crate::ActorEffectProfileWire",
                core_module: None,
            },
            crate::schema::ExternalType { haskell_name: "ActorEffectKey", rust_wire: "crate::ActorEffectKeyWire", core_module: None },
            crate::schema::ExternalType { haskell_name: "Model", rust_wire: "crate::Model", core_module: None },
            crate::schema::ExternalType { haskell_name: "ForkEffort", rust_wire: "crate::ForkEffort", core_module: None },
            crate::schema::ExternalType { haskell_name: "WorktreeSource", rust_wire: "tidepool_bridge_effects::WtWorktreeSource", core_module: None },
            crate::schema::ExternalType { haskell_name: "WorktreeHandle", rust_wire: "tidepool_bridge_effects::WtWorktreeHandle", core_module: None },
            crate::schema::ExternalType {
                haskell_name: "WorkerLifetime",
                rust_wire: "crate::WorkerLifetime",
                core_module: None,
            },
        ],
        errors: None,
        verbs: vec![
            Verb {
                ctor: "AgentLaunchSpawnWith",
                method: "agent_launch_spawn_with",
                args: vec![
                    installer_arg(),
                    external_arg("context", "SpawnContextWire"),
                    external_arg("workspace", "SpawnWorkspaceWire"),
                    Arg { name: "effects", ty: HsType::list(HsType::Named("ActorEffectKey")), rust: RustBinding::Path("Vec<crate::ActorEffectKeyWire>") },
                    optional_arg("label", HsType::Text, RustBinding::Derived),
                    optional_arg("model", HsType::Named("Model"), RustBinding::Path("Option<crate::Model>")),
                    optional_arg("effort", HsType::Named("ForkEffort"), RustBinding::Path("Option<crate::ForkEffort>")),
                    optional_arg("instructions", HsType::Text, RustBinding::Derived),
                    external_arg("lifetime", "WorkerLifetime"),
                    optional_arg("limits", HsType::Tuple(vec![HsType::Int, HsType::Int]), RustBinding::Derived),
                ],
                ret: HsType::either(HsType::Named("SpawnError"), HsType::Tuple(vec![HsType::Int, HsType::Int, HsType::maybe(HsType::Named("WorktreeHandle"))])),
                errors: None,
                handling: HandlingClass::Actor,
            },
            Verb {
                ctor: "AgentLaunchReplaceSpecWith",
                method: "agent_launch_replace_spec_with",
                args: vec![
                    Arg { name: "target", ty: HsType::Tuple(vec![HsType::Int, HsType::Int]), rust: RustBinding::Derived },
                    installer_arg(),
                    Arg { name: "effects", ty: HsType::list(HsType::Named("ActorEffectKey")), rust: RustBinding::Path("Vec<crate::ActorEffectKeyWire>") },
                ],
                ret: HsType::either(HsType::Named("SpecReplacementError"), HsType::Unit),
                errors: None,
                handling: HandlingClass::Actor,
            },
        ],
        helpers: Vec::new(),
        polymorphism: Polymorphism::None,
        generated_handler: false,
        handler_execution: crate::schema::HandlerExecution::Immediate,
        caller_principal: false,
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
            rust: RustBinding::HaskellValue,
        },
        // What this launch is, as data, alongside the opaque `entry` above.
        // `Just label` means the entry is exactly the stdlib's
        // `agentDefinitionUnbound label` (see `startForkedAgent` /
        // `Tidepool.Actors.Internal.Agent`) with no other captured runtime
        // binding beyond this text label; `Nothing` covers every other
        // launch shape (a bespoke `ActorDefinition`, an inline model-authored
        // body, ...), whose `entry` may close over arbitrary live state and
        // must keep running on the launching session. Rust decides per-child
        // session eligibility from this field, not by inspecting `entry`.
        Arg {
            name: "unboundLabel",
            ty: HsType::maybe(HsType::Text),
            rust: RustBinding::Derived,
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
            rust: RustBinding::External,
        },
        Arg {
            name: "profile",
            ty: HsType::Named("ActorEffectProfile"),
            rust: RustBinding::External,
        },
        Arg {
            name: "launchWorktrees",
            ty: HsType::List(Box::new(HsType::Text)),
            rust: RustBinding::Derived,
        },
    ]);
    if !forked {
        args.push(Arg {
            name: "lifetime",
            ty: HsType::Named("WorkerLifetime"),
            rust: RustBinding::External,
        });
    }
    args
}

pub(crate) fn launched_actor_type() -> HsType {
    HsType::Tuple(vec![HsType::Int, HsType::Int, HsType::Text])
}


fn installer_arg() -> Arg {
    Arg {
        name: "install",
        ty: HsType::func(HsType::Int, HsType::app(HsType::app(HsType::Named("Eff"), HsType::Var("childEffs")), HsType::Unit)),
        rust: RustBinding::HaskellValue,
    }
}

fn external_arg(name: &'static str, ty: &'static str) -> Arg {
    Arg { name, ty: HsType::Named(ty), rust: RustBinding::External }
}

fn optional_arg(name: &'static str, ty: HsType, rust: RustBinding) -> Arg {
    Arg { name, ty: HsType::maybe(ty), rust }
}

fn sum_type(name: &'static str, wire_rust: Option<&'static str>, variants: Vec<(&'static str, Vec<HsType>)>) -> TypeDef {
    TypeDef {
        name,
        wire_rust,
        haskell_module: None,
        shape: TypeShape::Sum { variants: variants.into_iter().map(|(ctor, fields)| SumVariant { ctor, fields: VariantFields::Positional(fields), doc: &[] }).collect() },
        json: JsonInstance::None,
        derives: WireDerives(&[]),
        domain: None,
        doc: &[],
    }
}

fn spawn_types() -> Vec<TypeDef> {
    vec![
        sum_type("SpawnContextWire", Some("crate::start::SpawnContextWire"), vec![("CapturedSpawn", vec![HsType::Text]), ("FreshSpawn", vec![HsType::Text])]),
        sum_type("WorkspaceHandle", Some("crate::fork_workspace::WorkspaceHandle"), vec![("WorkspaceHandle", vec![HsType::Text])]),
        sum_type("SpawnWorkspaceWire", Some("crate::fork_workspace::SpawnWorkspaceWire"), vec![("SameDirectory", vec![]), ("ExistingDirectory", vec![HsType::Named("WorkspaceHandle")]), ("ForkDirectory", vec![HsType::Named("WorktreeSource")])]),
        sum_type("SpawnError", None, vec![("SpawnRefused", vec![HsType::Text]), ("SpawnPartiallyStarted", vec![HsType::Tuple(vec![HsType::Int, HsType::Int]), HsType::maybe(HsType::Named("WorktreeHandle")), HsType::Text])]),
        sum_type("SpecReplacementError", None, vec![("SpecReplacementUnavailable", vec![]), ("SpecReplacementUnauthorized", vec![]), ("SpecReplacementSurfaceChanged", vec![]), ("SpecReplacementFailed", vec![HsType::Text])]),
    ]
}
