//! Nominal effect catalogue and ordered profiles. Interpreter availability and
//! concrete resource grants remain facts issued by runtime owners.
use crate::{
    gen::GeneratedFile,
    schema::{
        ExternalType, JsonInstance, SumVariant, TypeDef, TypeShape, VariantFields, WireDerives,
    },
};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelFamily {
    OpenAI,
    Argon,
    DeepSeek,
}

impl ModelFamily {
    pub const fn name(self) -> &'static str {
        match self {
            Self::OpenAI => "OpenAI",
            Self::Argon => "Argon",
            Self::DeepSeek => "DeepSeek",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelOperation {
    ModelCall,
    ForkAgent,
    SpawnAgent,
}

impl ModelOperation {
    pub const fn name(self) -> &'static str {
        match self {
            Self::ModelCall => "ModelCall",
            Self::ForkAgent => "ForkAgent",
            Self::SpawnAgent => "SpawnAgent",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelPermission {
    pub operation: ModelOperation,
    pub family: ModelFamily,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectScope {
    Delegable,
    InvocationOnly,
}

/// The declaration producer, independently of the emitted defining module.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeclarationOrigin {
    GeneratedCore,
    HandwrittenCore(HandwrittenCoreEffect),
    ExternalModule,
}

/// Core declarations owned by MCP's handwritten effect registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandwrittenCoreEffect {
    FsRead,
    FsWrite,
}

impl HandwrittenCoreEffect {
    const fn haskell_name(self) -> &'static str {
        match self {
            Self::FsRead => "FsRead",
            Self::FsWrite => "FsWrite",
        }
    }
}

/// A nominal declaration: defining module/type, key constructor, wire spelling,
/// classification and independent stable orders for each existing projection.
#[derive(Clone, Copy, Debug)]
pub struct NominalEffect {
    pub arguments: &'static [ExternalType],
    pub model_permission: Option<ModelPermission>,
    pub identity: ExternalType,
    pub origin: DeclarationOrigin,
    pub constructor: &'static str,
    pub wire_name: &'static str,
    pub scope: EffectScope,
    pub default_order: Option<usize>,
    pub rust_order: usize,
    pub bridge_order: usize,
}

impl NominalEffect {
    fn row_entry(&self) -> String {
        std::iter::once(self.identity.haskell_name)
            .chain(self.arguments.iter().map(|a| a.haskell_name))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

macro_rules! nominal {
    ($name:ident, $constructor:ident, $default:expr, $rust:expr, $bridge:expr, $module:expr) => {
        nominal!(
            $name,
            $constructor,
            $default,
            $rust,
            $bridge,
            $module,
            DeclarationOrigin::GeneratedCore
        )
    };
    ($name:ident, $constructor:ident, $default:expr, $rust:expr, $bridge:expr, $module:expr, $origin:expr) => {
        NominalEffect {
            arguments: &[],
            model_permission: None,
            identity: ExternalType {
                haskell_name: stringify!($name),
                rust_wire: stringify!($name),
                core_module: Some($module),
            },
            origin: $origin,
            constructor: stringify!($constructor),
            wire_name: stringify!($name),
            scope: EffectScope::Delegable,
            default_order: Some($default),
            rust_order: $rust,
            bridge_order: $bridge,
        }
    };
}

/// Catalogue order preserves the Haskell sum's existing constructor tags.
pub const EFFECTS: &[NominalEffect] = &[
    nominal!(
        ResourceScopes,
        EffectResourceScopes,
        2,
        1,
        0,
        "Tidepool.Effects.Core"
    ),
    nominal!(
        Replies,
        EffectReplies,
        0,
        0,
        1,
        "Tidepool.Agent.Reply.Internal",
        DeclarationOrigin::ExternalModule
    ),
    nominal!(
        Watches,
        EffectWatches,
        1,
        2,
        2,
        "Tidepool.Agent.Watch.Internal",
        DeclarationOrigin::ExternalModule
    ),
    nominal!(
        ActorContext,
        EffectActorContext,
        3,
        4,
        3,
        "Tidepool.Effects.Core"
    ),
    nominal!(
        AgentLaunch,
        EffectAgentLaunch,
        4,
        5,
        4,
        "Tidepool.Effects.Core"
    ),
    nominal!(
        AgentInspection,
        EffectAgentInspection,
        5,
        6,
        5,
        "Tidepool.Effects.Core"
    ),
    nominal!(
        AgentControl,
        EffectAgentControl,
        6,
        7,
        6,
        "Tidepool.Effects.Core"
    ),
    nominal!(
        BoundWorktree,
        EffectBoundWorktree,
        7,
        8,
        7,
        "Tidepool.Effects.Core"
    ),
    nominal!(
        WorktreeRegistry,
        EffectWorktreeRegistry,
        8,
        9,
        8,
        "Tidepool.Effects.Core"
    ),
    nominal!(
        WorktreeAllocation,
        EffectWorktreeAllocation,
        9,
        10,
        9,
        "Tidepool.Effects.Core"
    ),
    nominal!(
        WorktreeIntegration,
        EffectWorktreeIntegration,
        10,
        11,
        10,
        "Tidepool.Effects.Core"
    ),
    nominal!(Sleep, EffectSleep, 11, 12, 11, "Tidepool.Effects.Core"),
    nominal!(
        Commands,
        EffectCommands,
        14,
        16,
        15,
        "Tidepool.Effects.Core"
    ),
    nominal!(Console, EffectConsole, 15, 17, 16, "Tidepool.Effects.Core"),
    nominal!(AskUser, EffectAskUser, 16, 18, 17, "Tidepool.Effects.Core"),
    nominal!(Green, EffectGreen, 17, 19, 18, "Tidepool.Effects.Core"),
    nominal!(
        Notifications,
        EffectNotifications,
        12,
        13,
        12,
        "Tidepool.Effects.Core"
    ),
    nominal!(Jev, EffectJev, 13, 14, 13, "Tidepool.Effects.Core"),
    NominalEffect {
        identity: ExternalType {
            haskell_name: "ModelCall",
            rust_wire: "ModelCallOpenAI",
            core_module: Some("Tidepool.Effects.Core"),
        },
        arguments: &[ExternalType {
            haskell_name: "OpenAI",
            rust_wire: "OpenAI",
            core_module: Some("Tidepool.Effects.Core"),
        }],
        model_permission: Some(ModelPermission {
            operation: ModelOperation::ModelCall,
            family: ModelFamily::OpenAI,
        }),
        origin: DeclarationOrigin::GeneratedCore,
        constructor: "EffectModelCallOpenAI",
        wire_name: "ModelCall.OpenAI",
        scope: EffectScope::Delegable,
        default_order: None,
        rust_order: 31,
        bridge_order: 30,
    },
    NominalEffect {
        identity: ExternalType {
            haskell_name: "ModelCall",
            rust_wire: "ModelCallArgon",
            core_module: Some("Tidepool.Effects.Core"),
        },
        arguments: &[ExternalType {
            haskell_name: "Argon",
            rust_wire: "Argon",
            core_module: Some("Tidepool.Effects.Core"),
        }],
        model_permission: Some(ModelPermission {
            operation: ModelOperation::ModelCall,
            family: ModelFamily::Argon,
        }),
        origin: DeclarationOrigin::GeneratedCore,
        constructor: "EffectModelCallArgon",
        wire_name: "ModelCall.Argon",
        scope: EffectScope::Delegable,
        default_order: None,
        rust_order: 32,
        bridge_order: 31,
    },
    NominalEffect {
        identity: ExternalType {
            haskell_name: "ModelCall",
            rust_wire: "ModelCallDeepSeek",
            core_module: Some("Tidepool.Effects.Core"),
        },
        arguments: &[ExternalType {
            haskell_name: "DeepSeek",
            rust_wire: "DeepSeek",
            core_module: Some("Tidepool.Effects.Core"),
        }],
        model_permission: Some(ModelPermission {
            operation: ModelOperation::ModelCall,
            family: ModelFamily::DeepSeek,
        }),
        origin: DeclarationOrigin::GeneratedCore,
        constructor: "EffectModelCallDeepSeek",
        wire_name: "ModelCall.DeepSeek",
        scope: EffectScope::Delegable,
        default_order: None,
        rust_order: 33,
        bridge_order: 32,
    },
    NominalEffect {
        identity: ExternalType {
            haskell_name: "ForkAgent",
            rust_wire: "ForkAgentOpenAI",
            core_module: Some("Tidepool.Effects.Core"),
        },
        arguments: &[ExternalType {
            haskell_name: "OpenAI",
            rust_wire: "OpenAI",
            core_module: Some("Tidepool.Effects.Core"),
        }],
        model_permission: Some(ModelPermission {
            operation: ModelOperation::ForkAgent,
            family: ModelFamily::OpenAI,
        }),
        origin: DeclarationOrigin::GeneratedCore,
        constructor: "EffectForkAgentOpenAI",
        wire_name: "ForkAgent.OpenAI",
        scope: EffectScope::Delegable,
        default_order: None,
        rust_order: 34,
        bridge_order: 33,
    },
    NominalEffect {
        identity: ExternalType {
            haskell_name: "ForkAgent",
            rust_wire: "ForkAgentArgon",
            core_module: Some("Tidepool.Effects.Core"),
        },
        arguments: &[ExternalType {
            haskell_name: "Argon",
            rust_wire: "Argon",
            core_module: Some("Tidepool.Effects.Core"),
        }],
        model_permission: Some(ModelPermission {
            operation: ModelOperation::ForkAgent,
            family: ModelFamily::Argon,
        }),
        origin: DeclarationOrigin::GeneratedCore,
        constructor: "EffectForkAgentArgon",
        wire_name: "ForkAgent.Argon",
        scope: EffectScope::Delegable,
        default_order: None,
        rust_order: 35,
        bridge_order: 34,
    },
    NominalEffect {
        identity: ExternalType {
            haskell_name: "ForkAgent",
            rust_wire: "ForkAgentDeepSeek",
            core_module: Some("Tidepool.Effects.Core"),
        },
        arguments: &[ExternalType {
            haskell_name: "DeepSeek",
            rust_wire: "DeepSeek",
            core_module: Some("Tidepool.Effects.Core"),
        }],
        model_permission: Some(ModelPermission {
            operation: ModelOperation::ForkAgent,
            family: ModelFamily::DeepSeek,
        }),
        origin: DeclarationOrigin::GeneratedCore,
        constructor: "EffectForkAgentDeepSeek",
        wire_name: "ForkAgent.DeepSeek",
        scope: EffectScope::Delegable,
        default_order: None,
        rust_order: 36,
        bridge_order: 35,
    },
    NominalEffect {
        identity: ExternalType {
            haskell_name: "SpawnAgent",
            rust_wire: "SpawnAgentOpenAI",
            core_module: Some("Tidepool.Effects.Core"),
        },
        arguments: &[ExternalType {
            haskell_name: "OpenAI",
            rust_wire: "OpenAI",
            core_module: Some("Tidepool.Effects.Core"),
        }],
        model_permission: Some(ModelPermission {
            operation: ModelOperation::SpawnAgent,
            family: ModelFamily::OpenAI,
        }),
        origin: DeclarationOrigin::GeneratedCore,
        constructor: "EffectSpawnAgentOpenAI",
        wire_name: "SpawnAgent.OpenAI",
        scope: EffectScope::Delegable,
        default_order: None,
        rust_order: 37,
        bridge_order: 36,
    },
    NominalEffect {
        identity: ExternalType {
            haskell_name: "SpawnAgent",
            rust_wire: "SpawnAgentArgon",
            core_module: Some("Tidepool.Effects.Core"),
        },
        arguments: &[ExternalType {
            haskell_name: "Argon",
            rust_wire: "Argon",
            core_module: Some("Tidepool.Effects.Core"),
        }],
        model_permission: Some(ModelPermission {
            operation: ModelOperation::SpawnAgent,
            family: ModelFamily::Argon,
        }),
        origin: DeclarationOrigin::GeneratedCore,
        constructor: "EffectSpawnAgentArgon",
        wire_name: "SpawnAgent.Argon",
        scope: EffectScope::Delegable,
        default_order: None,
        rust_order: 38,
        bridge_order: 37,
    },
    NominalEffect {
        identity: ExternalType {
            haskell_name: "SpawnAgent",
            rust_wire: "SpawnAgentDeepSeek",
            core_module: Some("Tidepool.Effects.Core"),
        },
        arguments: &[ExternalType {
            haskell_name: "DeepSeek",
            rust_wire: "DeepSeek",
            core_module: Some("Tidepool.Effects.Core"),
        }],
        model_permission: Some(ModelPermission {
            operation: ModelOperation::SpawnAgent,
            family: ModelFamily::DeepSeek,
        }),
        origin: DeclarationOrigin::GeneratedCore,
        constructor: "EffectSpawnAgentDeepSeek",
        wire_name: "SpawnAgent.DeepSeek",
        scope: EffectScope::Delegable,
        default_order: None,
        rust_order: 39,
        bridge_order: 38,
    },
    nominal!(Actor, EffectActor, 18, 20, 19, "Tidepool.Effects.Core"),
    nominal!(Reflect, EffectReflect, 19, 21, 20, "Tidepool.Effects.Core"),
    nominal!(Lookup, EffectLookup, 20, 22, 21, "Tidepool.Effects.Core"),
    nominal!(
        RepoEvent,
        EffectRepoEvent,
        26,
        23,
        22,
        "Tidepool.Effects.Core"
    ),
    nominal!(Source, EffectSource, 21, 28, 23, "Tidepool.Effects.Core"),
    nominal!(Journal, EffectJournal, 22, 24, 24, "Tidepool.Effects.Core"),
    nominal!(Web, EffectWeb, 23, 25, 25, "Tidepool.Effects.Core"),
    nominal!(
        FsRead,
        EffectFsRead,
        24,
        26,
        26,
        "Tidepool.Effects.Core",
        DeclarationOrigin::HandwrittenCore(HandwrittenCoreEffect::FsRead)
    ),
    nominal!(
        FsWrite,
        EffectFsWrite,
        25,
        27,
        27,
        "Tidepool.Effects.Core",
        DeclarationOrigin::HandwrittenCore(HandwrittenCoreEffect::FsWrite)
    ),
    NominalEffect {
        arguments: &[],
        model_permission: None,
        identity: ExternalType {
            haskell_name: "ContextReadWrite",
            rust_wire: "ContextReadWrite",
            core_module: Some("Tidepool.Effects.Core"),
        },
        origin: DeclarationOrigin::GeneratedCore,
        constructor: "InvocationContextReadWrite",
        wire_name: "ContextReadWrite",
        scope: EffectScope::InvocationOnly,
        default_order: None,
        rust_order: 29,
        bridge_order: 28,
    },
    NominalEffect {
        arguments: &[],
        model_permission: None,
        identity: ExternalType {
            haskell_name: "Reload",
            rust_wire: "Reload",
            core_module: Some("Tidepool.Effects.Core"),
        },
        origin: DeclarationOrigin::GeneratedCore,
        constructor: "InvocationReload",
        wire_name: "Reload",
        scope: EffectScope::InvocationOnly,
        default_order: None,
        rust_order: 30,
        bridge_order: 29,
    },
];

/// Forks was serialized as an actor key but never had a Haskell nominal type.
/// Preserve decoding/re-encoding retained contracts; it has no witness, import,
/// default membership or interpreter support. New code uses AgentLaunch.
const LEGACY_ACTOR_KEYS: &[(&str, usize)] = &[("Forks", 3)];

fn delegable(effects: &[NominalEffect]) -> impl Iterator<Item = &NominalEffect> {
    effects
        .iter()
        .filter(|effect| effect.scope == EffectScope::Delegable)
}

fn defaults(effects: &[NominalEffect]) -> Vec<&NominalEffect> {
    let mut selected = effects
        .iter()
        .filter(|effect| effect.default_order.is_some())
        .collect::<Vec<_>>();
    selected.sort_by_key(|effect| effect.default_order);
    selected
}

fn validate(effects: &[NominalEffect]) -> Result<(), String> {
    let declarations = crate::effects::all();
    let mut names = BTreeSet::new();
    let mut constructors = BTreeSet::new();
    let mut wires = BTreeSet::new();
    let mut orders = BTreeSet::new();
    let mut ranks = BTreeSet::new();
    let mut bridge_orders = BTreeSet::new();
    for effect in effects {
        if !names.insert((
            effect.identity.core_module,
            effect.identity.haskell_name,
            effect
                .arguments
                .iter()
                .map(|a| (a.core_module, a.haskell_name))
                .collect::<Vec<_>>(),
        )) || !constructors.insert(effect.constructor)
            || !wires.insert(effect.wire_name)
            || !orders.insert(effect.rust_order)
            || !bridge_orders.insert(effect.bridge_order)
        {
            return Err(format!(
                "duplicate nominal effect identity/order: {}",
                effect.wire_name
            ));
        }
        let module = effect
            .identity
            .core_module
            .ok_or_else(|| format!("missing defining module: {}", effect.wire_name))?;
        let generated = declarations.iter().any(|declaration| {
            declaration.name == effect.identity.haskell_name
                && declaration.type_params.len() == effect.arguments.len()
        });
        let valid_origin = match effect.origin {
            DeclarationOrigin::GeneratedCore => module == "Tidepool.Effects.Core" && generated,
            DeclarationOrigin::HandwrittenCore(owner) => {
                module == "Tidepool.Effects.Core"
                    && effect.identity.haskell_name == owner.haskell_name()
                    && !generated
            }
            DeclarationOrigin::ExternalModule => module != "Tidepool.Effects.Core",
        };
        if !valid_origin {
            return Err(format!(
                "declaration origin {:?} does not issue {}.{}",
                effect.origin, module, effect.identity.haskell_name
            ));
        }
        if let Some(permission) = effect.model_permission {
            if effect.identity.haskell_name != permission.operation.name()
                || effect.arguments.len() != 1
                || effect.arguments[0].haskell_name != permission.family.name()
                || effect.arguments[0].core_module != Some("Tidepool.Effects.Core")
            {
                return Err(format!(
                    "model permission disagrees with nominal application: {}",
                    effect.wire_name
                ));
            }
        }
        if let Some(rank) = effect.default_order {
            if effect.scope != EffectScope::Delegable || !ranks.insert(rank) {
                return Err(format!(
                    "invalid default profile member: {}",
                    effect.wire_name
                ));
            }
        }
    }
    if ranks.iter().copied().ne(0..ranks.len()) {
        return Err("default profile order must be contiguous".into());
    }
    for (name, order) in LEGACY_ACTOR_KEYS {
        if wires.contains(name) || !orders.insert(*order) {
            return Err("legacy key collides with nominal catalogue".into());
        }
    }
    Ok(())
}

/// The AgentLaunch schema consumes this sum, rather than owning another list.
pub fn actor_key_definition() -> TypeDef {
    actor_key_definition_for(EFFECTS)
}

fn actor_key_definition_for(effects: &[NominalEffect]) -> TypeDef {
    TypeDef {
        name: "ActorEffectKey",
        wire_rust: None,
        haskell_module: None,
        shape: TypeShape::Sum {
            variants: delegable(effects)
                .map(|effect| SumVariant {
                    ctor: effect.constructor,
                    fields: VariantFields::Positional(Vec::new()),
                    doc: &[],
                })
                .collect(),
        },
        json: JsonInstance::None,
        derives: WireDerives(&[]),
        domain: None,
        doc: &["Stable nominal key for one generated model-facing effect."],
    }
}

fn imports(effects: &[NominalEffect]) -> String {
    let mut imports = BTreeSet::new();
    for effect in effects {
        imports.insert(effect.identity.core_import().expect("validated module"));
        for argument in effect.arguments {
            imports.insert(argument.core_import().expect("argument module"));
        }
    }
    imports.into_iter().collect::<Vec<_>>().join("\n")
}

fn render(effects: &[NominalEffect]) -> Vec<GeneratedFile> {
    let actor = delegable(effects).collect::<Vec<_>>();
    let invocation = effects
        .iter()
        .filter(|effect| effect.scope == EffectScope::InvocationOnly)
        .collect::<Vec<_>>();
    let mut rust = crate::gen::header("//! ", "Nominal effect identities");
    rust.push_str("\n#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]\npub enum ModelFamily { OpenAI, Argon, DeepSeek }\n\n#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]\n#[serde(rename_all = \"snake_case\")]\npub enum ModelOperation { ModelCall, ForkAgent, SpawnAgent }\n\n#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]\npub struct ModelPermission { pub operation: ModelOperation, pub family: ModelFamily }\n");
    rust.push_str("\n#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]\npub struct HaskellTypeIdentity {\n    pub module: &'static str,\n    pub name: &'static str,\n}\n\n#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]\npub struct HaskellEffectIdentity {\n    pub arguments: &'static [HaskellTypeIdentity],\n    pub module: &'static str,\n    pub name: &'static str,\n}\n\nimpl HaskellEffectIdentity {\n    pub fn render(self) -> String {\n        let mut rendered = format!(\"{}.{}\", self.module, self.name);\n        for argument in self.arguments {\n            rendered.push_str(&format!(\" {}.{}\", argument.module, argument.name));\n        }\n        rendered\n    }\n}\n\n#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]\npub enum ActorEffectKey {\n");
    let mut rust_keys = actor
        .iter()
        .map(|effect| {
            (
                effect.identity.rust_wire,
                effect.wire_name,
                effect.rust_order,
            )
        })
        .chain(
            LEGACY_ACTOR_KEYS
                .iter()
                .map(|(name, order)| (*name, *name, *order)),
        )
        .collect::<Vec<_>>();
    rust_keys.sort_by_key(|(_, _, order)| *order);
    for (name, wire, _) in &rust_keys {
        rust.push_str(&format!("    #[serde(rename = \"{wire}\")]\n    {name},\n"));
    }
    rust.push_str("}\n\nimpl ActorEffectKey {\n    pub const ALL: &'static [Self] = &[\n");
    for effect in &actor {
        rust.push_str(&format!("        Self::{},\n", effect.identity.rust_wire));
    }
    rust.push_str(
        "    ];\n\n    pub const fn haskell_name(self) -> &'static str {\n        match self {\n",
    );
    for (name, _, _) in &rust_keys {
        let nominal = effects
            .iter()
            .find(|effect| effect.identity.rust_wire == *name)
            .map_or(*name, |effect| effect.identity.haskell_name);
        rust.push_str(&format!("            Self::{name} => \"{nominal}\",\n"));
    }
    rust.push_str("        }\n    }\n\n    pub const fn wire_name(self) -> &'static str {\n        match self {\n");
    for (name, wire, _) in &rust_keys {
        rust.push_str(&format!("            Self::{name} => \"{wire}\",\n"));
    }
    rust.push_str("        }\n    }\n\n    pub const fn haskell_module(self) -> Option<&'static str> {\n        match self {\n");
    for effect in &actor {
        rust.push_str(&format!(
            "            Self::{} => Some(\"{}\"),\n",
            effect.identity.rust_wire,
            effect.identity.core_module.unwrap()
        ));
    }
    for (name, _) in LEGACY_ACTOR_KEYS {
        rust.push_str(&format!("            Self::{name} => None,\n"));
    }
    rust.push_str("        }\n    }\n\n    pub const fn haskell_constructor(self) -> Option<&'static str> {\n        match self {\n");
    for effect in &actor {
        rust.push_str(&format!(
            "            Self::{} => Some(\"{}\"),\n",
            effect.identity.rust_wire, effect.constructor
        ));
    }
    for (name, _) in LEGACY_ACTOR_KEYS {
        rust.push_str(&format!("            Self::{name} => None,\n"));
    }
    rust.push_str("        }\n    }\n");
    rust.push_str("\n    pub const fn haskell_arguments(self) -> &'static [HaskellTypeIdentity] {\n        match self {\n");
    for effect in &actor {
        rust.push_str(&format!(
            "            Self::{} => &[",
            effect.identity.rust_wire
        ));
        for arg in effect.arguments {
            rust.push_str(&format!(
                "HaskellTypeIdentity {{ module: \"{}\", name: \"{}\" }},",
                arg.core_module.unwrap(),
                arg.haskell_name
            ));
        }
        rust.push_str("],\n");
    }
    for (name, _) in LEGACY_ACTOR_KEYS {
        rust.push_str(&format!("            Self::{name} => &[],\n"));
    }
    rust.push_str("        }\n    }\n\n    pub const fn model_permission(self) -> Option<ModelPermission> {\n        match self {\n");
    for effect in &actor {
        if let Some(permission) = effect.model_permission {
            rust.push_str(&format!("            Self::{} => Some(ModelPermission {{ operation: ModelOperation::{}, family: ModelFamily::{} }}),\n", effect.identity.rust_wire, permission.operation.name(), permission.family.name()));
        }
    }
    rust.push_str("            _ => None,\n");
    rust.push_str("        }\n    }\n\n    pub const fn nominal_identity(self) -> Option<HaskellEffectIdentity> {\n        match self.haskell_module() {\n            Some(module) => Some(HaskellEffectIdentity {\n                module,\n                name: self.haskell_name(),\n                arguments: self.haskell_arguments(),\n            }),\n            None => None,\n        }\n    }\n}\n\n#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]\npub enum ToolEffectKey {\n    Actor(ActorEffectKey),\n");
    for effect in &invocation {
        rust.push_str(&format!("    {},\n", effect.identity.rust_wire));
    }
    rust.push_str("}\n\nimpl ToolEffectKey {\n    pub const ALL: &'static [Self] = &[\n");
    for effect in &actor {
        rust.push_str(&format!(
            "        Self::Actor(ActorEffectKey::{}),\n",
            effect.identity.rust_wire
        ));
    }
    for effect in &invocation {
        rust.push_str(&format!("        Self::{},\n", effect.identity.rust_wire));
    }
    rust.push_str("    ];\n\n    pub const fn haskell_name(self) -> &'static str {\n        match self {\n            Self::Actor(key) => key.haskell_name(),\n");
    for effect in &invocation {
        rust.push_str(&format!(
            "            Self::{} => \"{}\",\n",
            effect.identity.rust_wire, effect.identity.haskell_name
        ));
    }
    rust.push_str("        }\n    }\n\n    pub const fn wire_name(self) -> &'static str {\n        match self {\n            Self::Actor(key) => key.wire_name(),\n");
    for effect in &invocation {
        rust.push_str(&format!(
            "            Self::{} => \"{}\",\n",
            effect.identity.rust_wire, effect.wire_name
        ));
    }
    rust.push_str("        }\n    }\n\n    pub const fn haskell_module(self) -> Option<&'static str> {\n        match self {\n            Self::Actor(key) => key.haskell_module(),\n");
    for effect in &invocation {
        rust.push_str(&format!(
            "            Self::{} => Some(\"{}\"),\n",
            effect.identity.rust_wire,
            effect.identity.core_module.unwrap()
        ));
    }
    rust.push_str("        }\n    }\n\n    pub const fn is_invocation_only(self) -> bool {\n        match self {\n            Self::Actor(_) => false,\n");
    for effect in &invocation {
        rust.push_str(&format!(
            "            Self::{} => true,\n",
            effect.identity.rust_wire
        ));
    }
    rust.push_str("        }\n    }\n\n    pub const fn haskell_arguments(self) -> &'static [HaskellTypeIdentity] {\n        match self {\n            Self::Actor(key) => key.haskell_arguments(),\n            _ => &[],\n");
    rust.push_str("        }\n    }\n\n    pub const fn nominal_identity(self) -> Option<HaskellEffectIdentity> {\n        match self.haskell_module() {\n            Some(module) => Some(HaskellEffectIdentity {\n                module,\n                name: self.haskell_name(),\n                arguments: self.haskell_arguments(),\n            }),\n            None => None,\n        }\n    }\n}\n\nimpl From<ActorEffectKey> for ToolEffectKey {\n    fn from(key: ActorEffectKey) -> Self {\n        Self::Actor(key)\n    }\n}\n\nimpl serde::Serialize for ToolEffectKey {\n    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {\n        serializer.serialize_str(self.wire_name())\n    }\n}\n\nimpl<'de> serde::Deserialize<'de> for ToolEffectKey {\n    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {\n        let name = <String as serde::Deserialize>::deserialize(deserializer)?;\n        match name.as_str() {\n");
    for effect in &invocation {
        rust.push_str(&format!(
            "            \"{}\" => Ok(Self::{}),\n",
            effect.wire_name, effect.identity.rust_wire
        ));
    }
    rust.push_str("            _ => ActorEffectKey::deserialize(serde::de::value::StrDeserializer::<D::Error>::new(\n                &name,\n            ))\n            .map(Self::Actor),\n        }\n    }\n}\n");
    let mut bridge = crate::gen::header("//! ", "Nominal actor effect key bridge");
    bridge.push_str("\n#[derive(Debug, Clone, Copy, PartialEq, Eq, tidepool_bridge_derive::FromHaskell)]\npub enum ActorEffectKeyWire {\n");
    let mut bridge_effects = actor.clone();
    bridge_effects.sort_by_key(|effect| effect.bridge_order);
    for effect in &bridge_effects {
        bridge.push_str(&format!(
            "    #[haskell(module = \"Tidepool.Effects.Core\")]\n    {},\n",
            effect.constructor
        ));
    }
    bridge.push_str("}\n\nimpl ActorEffectKeyWire {\n    pub const ALL: &'static [Self] = &[\n");
    for effect in &actor {
        bridge.push_str(&format!("        Self::{},\n", effect.constructor));
    }
    bridge.push_str("    ];\n\n    pub const fn haskell_constructor(self) -> &'static str {\n        match self {\n");
    for effect in &bridge_effects {
        bridge.push_str(&format!(
            "            Self::{} => \"{}\",\n",
            effect.constructor, effect.constructor
        ));
    }
    bridge.push_str("        }\n    }\n}\n\nimpl From<ActorEffectKeyWire> for crate::ActorEffectKey {\n    fn from(value: ActorEffectKeyWire) -> Self {\n        match value {\n");
    for effect in &bridge_effects {
        bridge.push_str(&format!(
            "            ActorEffectKeyWire::{} => Self::{},\n",
            effect.constructor, effect.identity.rust_wire
        ));
    }
    bridge.push_str("        }\n    }\n}\n");
    let mut profiles = crate::gen::header("//! ", "Default actor effects");
    profiles.push_str(
        "\nuse crate::ActorEffectKey;\n\npub const DEFAULT_ACTOR_EFFECTS: &[ActorEffectKey] = &[\n",
    );
    for effect in defaults(effects) {
        profiles.push_str(&format!(
            "    ActorEffectKey::{},\n",
            effect.identity.rust_wire
        ));
    }
    profiles.push_str("];\n");
    let mut hs = "{-# LANGUAGE ConstraintKinds #-}\n{-# LANGUAGE DataKinds #-}\n{-# LANGUAGE FlexibleInstances #-}\n{-# LANGUAGE KindSignatures #-}\n{-# LANGUAGE OverloadedStrings #-}\n{-# LANGUAGE ScopedTypeVariables #-}\n{-# LANGUAGE TypeApplications #-}\n{-# LANGUAGE TypeFamilies #-}\n{-# LANGUAGE TypeOperators #-}\n{-# LANGUAGE UndecidableInstances #-}\n".to_owned();
    hs.push_str(&crate::gen::header(
        "-- ",
        "Nominal effect identities and profiles",
    ));
    hs.push_str("module Tidepool.Internal.ActorProfiles\n  ( ActorEffects, CatalogueEffects, CatalogueToolEffects, ToolEffectKey (..), actorEffectName, toolEffectName\n  , KnownToolEffect (knownToolEffectName), AsyncEffects, SyncEffects, raiseSync ) where\n\nimport Control.Monad.Freer (Eff, raise)\nimport Data.Kind (Constraint, Type)\nimport Data.Proxy (Proxy (..))\nimport Data.Text (Text)\nimport GHC.TypeLits (ErrorMessage (..), TypeError)\nimport Tidepool.Effects.Core (ActorEffectKey (..))\nimport Tidepool.Effects.Row (KnownEffect, KnownEffects (knownEffects), effectKeys)\n");
    hs.push_str(&imports(effects));
    hs.push_str("\n\ntype ActorEffects =\n  '[");
    hs.push_str(
        &defaults(effects)
            .iter()
            .map(|effect| effect.row_entry())
            .collect::<Vec<_>>()
            .join(", "),
    );
    hs.push_str("]\n\ntype CatalogueEffects =\n  '[");
    hs.push_str(
        &actor
            .iter()
            .map(|effect| effect.row_entry())
            .collect::<Vec<_>>()
            .join(", "),
    );
    hs.push_str("]\n\ntype CatalogueToolEffects =\n  '[");
    hs.push_str(
        &effects
            .iter()
            .map(|effect| effect.row_entry())
            .collect::<Vec<_>>()
            .join(", "),
    );
    hs.push_str("]\n\ndata ToolEffectKey = ActorEffect ActorEffectKey");
    for effect in &invocation {
        hs.push_str(&format!(" | {}", effect.constructor));
    }
    hs.push_str("\n  deriving (Eq, Show)\n\nactorEffectName :: ActorEffectKey -> Text\nactorEffectName key = case key of\n");
    for effect in &actor {
        hs.push_str(&format!(
            "  {} -> \"{}\"\n",
            effect.constructor, effect.wire_name
        ));
    }
    hs.push_str("\ntoolEffectName :: ToolEffectKey -> Text\ntoolEffectName key = case key of\n  ActorEffect actor -> actorEffectName actor\n");
    for effect in &invocation {
        hs.push_str(&format!(
            "  {} -> \"{}\"\n",
            effect.constructor, effect.wire_name
        ));
    }
    hs.push_str("\nclass KnownToolEffect (effect :: Type -> Type) where\n  knownToolEffectName :: Proxy effect -> Text\n\ninstance {-# OVERLAPPABLE #-} KnownEffect effect => KnownToolEffect effect where\n  knownToolEffectName _ = case effectKeys (knownEffects @'[effect]) of\n    [key] -> actorEffectName key\n    _ -> error \"a singleton effect witness must name one effect\"\n");
    for effect in &invocation {
        hs.push_str(&format!("\ninstance {{-# OVERLAPPING #-}} KnownToolEffect {} where\n  knownToolEffectName _ = toolEffectName {}\n", effect.identity.haskell_name, effect.constructor));
    }
    hs.push_str("\ntype SyncEffects effects = ");
    for effect in &invocation {
        hs.push_str(&format!("{} ': ", effect.identity.haskell_name));
    }
    hs.push_str("effects\n");
    hs.push_str("\nraiseSync :: Eff effects a -> Eff (SyncEffects effects) a\nraiseSync = ");
    for _ in &invocation {
        hs.push_str("raise . ");
    }
    hs.push_str("id\n");
    hs.push_str("\ntype family AsyncEffects (effects :: [Type -> Type]) :: Constraint where\n  AsyncEffects '[] = ()\n");
    for effect in &invocation {
        hs.push_str(&format!("  AsyncEffects ({} ': effects) =\n    TypeError ('Text \"{} cannot be used by an asynchronous tool, model callback, or hook.\")\n", effect.identity.haskell_name, effect.identity.haskell_name));
    }
    hs.push_str("  AsyncEffects (effect ': effects) = AsyncEffects effects\n");
    let mut row = "{-# LANGUAGE DataKinds #-}\n{-# LANGUAGE FlexibleInstances #-}\n{-# LANGUAGE GADTs #-}\n{-# LANGUAGE KindSignatures #-}\n{-# LANGUAGE MultiParamTypeClasses #-}\n{-# LANGUAGE RoleAnnotations #-}\n{-# LANGUAGE TypeOperators #-}\n{-# LANGUAGE UndecidableInstances #-}\n".to_owned();
    row.push_str(&crate::gen::header("-- ", "Effect row witnesses"));
    row.push_str("module Tidepool.Effects.Row\n  ( EffectWitness, Effects, KnownEffect (effectWitness), KnownEffects (knownEffects)\n  , effectKeys, Subset\n  ) where\n\nimport Data.Kind (Type)\nimport Tidepool.Effects.Core (ActorEffectKey (..))\n");
    row.push_str(&imports(
        &actor.iter().map(|effect| **effect).collect::<Vec<_>>(),
    ));
    row.push_str("\n\n");
    row.push_str(ROW_WITNESS_INFRASTRUCTURE);
    for effect in &actor {
        row.push_str(&format!(
            "instance KnownEffect ({}) where effectWitness = EffectWitness {}\n",
            effect.row_entry(),
            effect.constructor
        ));
    }

    vec![
        GeneratedFile {
            path: "exomonad/actor/src/generated/nominal_keys.rs".into(),
            contents: bridge,
        },
        GeneratedFile {
            path: "exomonad/tool/src/generated/effect_keys.rs".into(),
            contents: rust,
        },
        GeneratedFile {
            path: "exomonad/tool/src/generated/public_profiles.rs".into(),
            contents: profiles,
        },
        GeneratedFile {
            path: "bridge/haskell/lib/Tidepool/Internal/ActorProfiles.hs".into(),
            contents: hs,
        },
        GeneratedFile {
            path: "bridge/haskell/lib/Tidepool/Effects/Row.hs".into(),
            contents: row,
        },
    ]
}

#[must_use]
pub fn generated_files() -> Vec<GeneratedFile> {
    validate(EFFECTS).unwrap_or_else(|error| panic!("invalid nominal effect catalogue: {error}"));
    render(EFFECTS)
}

const ROW_WITNESS_INFRASTRUCTURE: &str = r#"type role EffectWitness nominal

data EffectWitness (effect :: Type -> Type) = EffectWitness ActorEffectKey

data Effects (effects :: [Type -> Type]) where
  ENil :: Effects '[]
  ECons :: EffectWitness effect -> Effects effects -> Effects (effect ': effects)

class KnownEffect (effect :: Type -> Type) where
  effectWitness :: EffectWitness effect

class KnownEffects (effects :: [Type -> Type]) where
  knownEffects :: Effects effects

instance KnownEffects '[] where
  knownEffects = ENil

instance (KnownEffect effect, KnownEffects effects) => KnownEffects (effect ': effects) where
  knownEffects = ECons effectWitness knownEffects

effectKeys :: Effects effects -> [ActorEffectKey]
effectKeys ENil = []
effectKeys (ECons (EffectWitness key) rest) = key : effectKeys rest

class Contains (effect :: Type -> Type) (effects :: [Type -> Type])
instance {-# OVERLAPPING #-} Contains effect (effect ': effects)
instance {-# OVERLAPPABLE #-} Contains effect effects => Contains effect (other ': effects)

class Subset (child :: [Type -> Type]) (parent :: [Type -> Type])
instance Subset '[] parent
instance (Contains effect parent, Subset effects parent) => Subset (effect ': effects) parent

"#;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalogue_rejects_duplicates_unknown_core_and_invocation_defaults() {
        assert!(validate(EFFECTS).is_ok());
        let mut changed = EFFECTS.to_vec();
        changed.push(changed[0]);
        assert!(validate(&changed).is_err());
        let mut changed = EFFECTS.to_vec();
        changed[0].identity.haskell_name = "NotAnEffect";
        assert!(validate(&changed).is_err());
        let mut changed = EFFECTS.to_vec();
        changed.last_mut().unwrap().default_order = Some(25);
        assert!(validate(&changed).is_err());
    }

    #[test]
    fn handwritten_core_identity_requires_its_exact_declaration_origin() {
        assert!(validate(EFFECTS).is_ok());
        for owner in [
            HandwrittenCoreEffect::FsRead,
            HandwrittenCoreEffect::FsWrite,
        ] {
            let index = EFFECTS
                .iter()
                .position(|effect| effect.origin == DeclarationOrigin::HandwrittenCore(owner))
                .unwrap();
            for wrong_origin in [
                DeclarationOrigin::GeneratedCore,
                DeclarationOrigin::ExternalModule,
            ] {
                let mut changed = EFFECTS.to_vec();
                changed[index].origin = wrong_origin;
                assert!(validate(&changed).is_err());
            }
            let mut changed = EFFECTS.to_vec();
            changed[index].identity.haskell_name = "NotAnEffect";
            assert!(validate(&changed).is_err());
            let mut changed = EFFECTS.to_vec();
            changed[index].identity.core_module = Some("Foreign.Core");
            assert!(validate(&changed).is_err());
        }
        let mut changed = EFFECTS.to_vec();
        changed[0].origin = DeclarationOrigin::HandwrittenCore(HandwrittenCoreEffect::FsRead);
        assert!(validate(&changed).is_err());
    }

    #[test]
    fn invocation_catalogue_mutation_generates_witnesses_and_async_refusal_without_delegation() {
        let mut changed = EFFECTS.to_vec();
        changed.push(NominalEffect {
            arguments: &[],
            model_permission: None,
            identity: ExternalType {
                haskell_name: "InvocationProbe",
                rust_wire: "InvocationProbe",
                core_module: Some("Example.Invocation"),
            },
            origin: DeclarationOrigin::ExternalModule,
            constructor: "InvocationProbeKey",
            wire_name: "InvocationProbeWire",
            scope: EffectScope::InvocationOnly,
            default_order: None,
            rust_order: EFFECTS
                .iter()
                .map(|effect| effect.rust_order)
                .max()
                .unwrap()
                + 1,
            bridge_order: EFFECTS
                .iter()
                .map(|effect| effect.bridge_order)
                .max()
                .unwrap()
                + 1,
        });
        assert!(validate(&changed).is_ok());
        for file in render(&changed) {
            if file.path.ends_with("ActorProfiles.hs") {
                assert!(file.contents.contains("KnownToolEffect InvocationProbe"));
                assert!(file
                    .contents
                    .contains("ContextReadWrite ': Reload ': InvocationProbe ': effects"));
                assert!(file
                    .contents
                    .contains("AsyncEffects (InvocationProbe ': effects)"));
                assert!(file
                    .contents
                    .contains("Example.Invocation (InvocationProbe)"));
            } else if file.path.ends_with("effect_keys.rs") {
                assert!(file.contents.contains("Self::InvocationProbe => true"));
                let actor_enum = file
                    .contents
                    .split("pub enum ActorEffectKey {")
                    .nth(1)
                    .unwrap()
                    .split('}')
                    .next()
                    .unwrap();
                assert!(!actor_enum.contains("InvocationProbe"));
            } else {
                assert!(
                    !file.contents.contains("InvocationProbe"),
                    "{} promoted invocation effect",
                    file.path
                );
            }
        }
        let TypeShape::Sum { variants } = actor_key_definition_for(&changed).shape else {
            panic!()
        };
        assert!(variants
            .iter()
            .all(|variant| variant.ctor != "InvocationProbeKey"));
    }

    #[test]
    fn catalogue_mutations_reach_every_projection_without_another_table() {
        let original = render(EFFECTS);
        // A nominal external entry can be added without a second Core declaration.
        let added = NominalEffect {
            arguments: &[],
            model_permission: None,
            identity: ExternalType {
                haskell_name: "Probe",
                rust_wire: "Probe",
                core_module: Some("Example.Probe"),
            },
            origin: DeclarationOrigin::ExternalModule,
            constructor: "EffectProbe",
            wire_name: "ProbeWire",
            scope: EffectScope::Delegable,
            default_order: Some(defaults(EFFECTS).len()),
            rust_order: EFFECTS
                .iter()
                .map(|effect| effect.rust_order)
                .max()
                .unwrap()
                + 1,
            bridge_order: EFFECTS
                .iter()
                .map(|effect| effect.bridge_order)
                .max()
                .unwrap()
                + 1,
        };
        let mut changed = EFFECTS.to_vec();
        changed.push(added);
        assert!(validate(&changed).is_ok());
        for file in render(&changed) {
            assert!(
                file.contents.contains("Probe"),
                "{} missed addition",
                file.path
            );
        }
        let TypeShape::Sum { variants } = actor_key_definition_for(&changed).shape else {
            panic!()
        };
        assert_eq!(variants.last().unwrap().ctor, "EffectProbe");
        changed.pop();
        assert_eq!(
            render(&changed)
                .iter()
                .map(|file| (&file.path, &file.contents))
                .collect::<Vec<_>>(),
            original
                .iter()
                .map(|file| (&file.path, &file.contents))
                .collect::<Vec<_>>()
        );
        let target = &mut changed[1];
        target.identity = ExternalType {
            haskell_name: "RenamedReplies",
            rust_wire: "RenamedReplies",
            core_module: Some("Example.Renamed"),
        };
        target.constructor = "EffectRenamedReplies";
        target.wire_name = "RenamedRepliesWire";
        assert!(validate(&changed).is_ok());
        for file in render(&changed) {
            assert!(
                file.contents.contains("RenamedReplies"),
                "{} missed rename",
                file.path
            );
            assert!(
                !file.contents.contains("EffectReplies"),
                "{} retained old key",
                file.path
            );
        }
        changed.remove(1);
        for effect in &mut changed {
            if let Some(rank) = &mut effect.default_order {
                if *rank > 0 {
                    *rank -= 1;
                }
            }
        }
        assert!(validate(&changed).is_ok());
        for file in render(&changed) {
            assert!(
                !file.contents.contains("RenamedReplies"),
                "{} retained removal",
                file.path
            );
        }
    }
}

/// Indexed witness retains the family through the erased STG request boundary.
pub fn family_vocabulary() -> String {
    let families = [
        ModelFamily::OpenAI,
        ModelFamily::Argon,
        ModelFamily::DeepSeek,
    ];
    let mut hs = String::new();
    for family in families {
        hs.push_str(&format!("data {}\n", family.name()));
    }
    hs.push_str("type role FamilyWitness nominal\ndata FamilyWitness family where\n");
    for family in families {
        hs.push_str(&format!(
            "  {}Witness :: FamilyWitness {}\n",
            family.name(),
            family.name()
        ));
    }
    hs.push_str("class KnownFamily family where\n  familyWitness :: FamilyWitness family\n");
    for family in families {
        hs.push_str(&format!(
            "instance KnownFamily {} where familyWitness = {}Witness\n",
            family.name(),
            family.name()
        ));
    }
    hs
}

pub fn family_witness_rust() -> String {
    String::from("\n#[derive(Debug, Clone, Copy, PartialEq, Eq, tidepool_bridge_derive::FromHaskell)]\npub enum ModelFamilyWitness {\n    #[haskell(module = \"Tidepool.Effects.Core\")]\n    OpenAIWitness,\n    #[haskell(module = \"Tidepool.Effects.Core\")]\n    ArgonWitness,\n    #[haskell(module = \"Tidepool.Effects.Core\")]\n    DeepSeekWitness,\n}\n\nimpl From<ModelFamilyWitness> for exomonad_tool::ModelFamily {\n    fn from(value: ModelFamilyWitness) -> Self {\n        match value {\n            ModelFamilyWitness::OpenAIWitness => Self::OpenAI,\n            ModelFamilyWitness::ArgonWitness => Self::Argon,\n            ModelFamilyWitness::DeepSeekWitness => Self::DeepSeek,\n        }\n    }\n}\n")
}
