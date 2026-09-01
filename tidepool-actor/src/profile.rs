use serde::{Deserialize, Serialize};

/// Experimental named resident-Haskell effect surface for one incarnation.
///
/// These profiles exercise static row selection, spawn attenuation, and
/// interpreter authorization. They are not operating-system sandboxes and do
/// not constrain native tools exposed by an attached coding-agent process.
/// Resource grants and process isolation remain orthogonal runtime concerns.
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

    /// Nominal effect names in the Haskell row represented by this profile,
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
                "Worktree",
            ],
            Self::ReadOnly => &[
                "ActorKernel",
                "ActorLocal",
                "ActorMcp",
                "Actor",
                "Deliberate",
                "FsRead",
                "Worktree",
            ],
        }
    }
}
