use serde::{Deserialize, Serialize};

/// Experimental named resident-Haskell effect surface for one incarnation.
///
/// These profiles exercise static row selection and spawn attenuation. They
/// are not operating-system sandboxes and do not constrain native tools
/// exposed by an attached coding-agent process. Resource grants, interpreter
/// authorization, and process isolation remain orthogonal runtime concerns.
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
}

#[cfg(test)]
mod tests {
    use super::ActorEffectProfile::{ReadOnly, ReadWrite};

    #[test]
    fn spawn_profiles_only_attenuate() {
        assert!(ReadWrite.permits_child(ReadWrite));
        assert!(ReadWrite.permits_child(ReadOnly));
        assert!(ReadOnly.permits_child(ReadOnly));
        assert!(!ReadOnly.permits_child(ReadWrite));
    }
}
