//! Effect identities shared by tool contracts and interpreter installation.
//! Context mutation is invocation-scoped and deliberately absent from the
//! actor-delegable effect keys.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum ActorEffectKey {
    Replies,
    ResourceScopes,
    Watches,
    Forks,
    ActorContext,
    AgentLaunch,
    AgentInspection,
    AgentControl,
    BoundWorktree,
    WorktreeRegistry,
    WorktreeAllocation,
    WorktreeIntegration,
    Sleep,
    Notifications,
    Jev,
    ModelCall,
    Commands,
    Console,
    Actor,
    Reflect,
    Lookup,
    RepoEvent,
    Journal,
    /// Reloading the source layer the holder's own cells compile against. The
    /// root's layer is the run's; an actor launched with a checkout has its
    /// own, captured from that checkout. The key names the verb, never the
    /// layer: which layer a call reaches is fixed when the actor is built, so
    /// holding this key in a checkout cannot reach the run's source.
    Source,
}

impl ActorEffectKey {
    pub const fn haskell_name(self) -> &'static str {
        match self {
            Self::Replies => "Replies",
            Self::ResourceScopes => "ResourceScopes",
            Self::Watches => "Watches",
            Self::Forks => "Forks",
            Self::ActorContext => "ActorContext",
            Self::AgentLaunch => "AgentLaunch",
            Self::AgentInspection => "AgentInspection",
            Self::AgentControl => "AgentControl",
            Self::BoundWorktree => "BoundWorktree",
            Self::WorktreeRegistry => "WorktreeRegistry",
            Self::WorktreeAllocation => "WorktreeAllocation",
            Self::WorktreeIntegration => "WorktreeIntegration",
            Self::Sleep => "Sleep",
            Self::Notifications => "Notifications",
            Self::Jev => "Jev",
            Self::ModelCall => "ModelCall",
            Self::Commands => "Commands",
            Self::Console => "Console",
            Self::Actor => "Actor",
            Self::Reflect => "Reflect",
            Self::Lookup => "Lookup",
            Self::RepoEvent => "RepoEvent",
            Self::Journal => "Journal",
            Self::Source => "Source",
        }
    }
}

/// An effect a resident tool can use, including invocation-scoped families.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolEffectKey {
    Actor(ActorEffectKey),
    ContextReadWrite,
}

impl ToolEffectKey {
    #[must_use]
    pub const fn haskell_name(self) -> &'static str {
        match self {
            Self::Actor(key) => key.haskell_name(),
            Self::ContextReadWrite => "ContextReadWrite",
        }
    }
}

impl From<ActorEffectKey> for ToolEffectKey {
    fn from(key: ActorEffectKey) -> Self {
        Self::Actor(key)
    }
}

impl serde::Serialize for ToolEffectKey {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.haskell_name())
    }
}

impl<'de> serde::Deserialize<'de> for ToolEffectKey {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = <String as serde::Deserialize>::deserialize(deserializer)?;
        if name == "ContextReadWrite" {
            return Ok(Self::ContextReadWrite);
        }
        let key =
            ActorEffectKey::deserialize(serde::de::value::StrDeserializer::<D::Error>::new(&name))?;
        Ok(Self::Actor(key))
    }
}
