//! Independent idle subagent admission and compiled installation replacement.

use crate::hs::HsType;
use crate::schema::{Arg, Effect, HandlingClass, JsonInstance, Polymorphism, RustBinding, SumVariant, TypeDef, TypeShape, VariantFields, Verb, WireDerives};

/// Admit an idle persistent actor with explicit context, directory and tools.
#[must_use]
pub fn agent_launch() -> Effect {
    Effect {
        name: "AgentLaunch",
        authored_surface: crate::schema::AuthoredSurface::OPAQUE,
        handler: "AgentLaunchDecodeHandler",
        handler_module: "agent_launch",
        req_enum: "AgentLaunchReq",
        decl_fn: "agent_launch_decl",
        description: &["Independent idle subagent admission with compiled tools and explicit context, workspace and cleanup ownership."],
        prompt_card: None,
        type_params: &[],
        default_row_args: &[],
        extra_imports: &[],
        type_defs: spawn_types(),
        external_types: &[
            crate::schema::ExternalType { haskell_name: "ActorEffectKey", rust_wire: "crate::ActorEffectKeyWire", core_module: None },
            crate::schema::ExternalType { haskell_name: "Model", rust_wire: "crate::Model", core_module: None },
            crate::schema::ExternalType { haskell_name: "ForkEffort", rust_wire: "crate::ForkEffort", core_module: None },
            crate::schema::ExternalType { haskell_name: "WorkspaceHandle", rust_wire: "tidepool_bridge_effects::WtWorkspaceHandle", core_module: None },
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
                    external_arg("context", "SpawnContextWire"),
                    installer_arg(),
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
        sum_type("SpawnWorkspaceWire", Some("crate::fork_workspace::SpawnWorkspaceWire"), vec![("SameDirectory", vec![]), ("ExistingDirectory", vec![HsType::Named("WorkspaceHandle")]), ("ForkDirectory", vec![HsType::Named("WorktreeSource")])]),
        sum_type("SpawnError", None, vec![("SpawnRefused", vec![HsType::Text]), ("SpawnPartiallyStarted", vec![HsType::Tuple(vec![HsType::Int, HsType::Int]), HsType::maybe(HsType::Named("WorktreeHandle")), HsType::Text])]),
        sum_type("SpecReplacementError", None, vec![("SpecReplacementUnavailable", vec![]), ("SpecReplacementUnauthorized", vec![]), ("SpecReplacementSurfaceChanged", vec![]), ("SpecReplacementFailed", vec![HsType::Text])]),
    ]
}
