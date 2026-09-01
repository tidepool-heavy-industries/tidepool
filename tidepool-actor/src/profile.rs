use serde::{Deserialize, Serialize};

/// Named model-facing effect surface selected for one actor incarnation.
///
/// Resource grants remain orthogonal. A profile constrains which operation
/// classes an actor may express and which child profiles it may start.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorEffectProfile {
    #[default]
    ReadWrite,
    ReadOnly,
}

impl ActorEffectProfile {
    #[must_use]
    pub const fn permits_child(self, child: Self) -> bool {
        matches!(
            (self, child),
            (Self::ReadWrite, _) | (Self::ReadOnly, Self::ReadOnly)
        )
    }

    /// Nominal effect names in the exact Haskell row selected by this profile,
    /// including the kernel-private outer entry row.
    #[must_use]
    pub const fn effect_names(self) -> &'static [&'static str] {
        match self {
            Self::ReadWrite => &[
                "ActorKernel",
                "FsWrite",
                "ActorLocal",
                "ActorMcp",
                "Actor",
                "Deliberate",
                "FsRead",
            ],
            Self::ReadOnly => &[
                "ActorKernel",
                "ActorLocal",
                "ActorMcp",
                "Actor",
                "Deliberate",
                "FsRead",
            ],
        }
    }
}
