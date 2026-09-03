use tidepool_model::Role;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromptId {
    SystemFraming,
}

impl PromptId {
    #[cfg(test)]
    const ALL: [Self; 1] = [Self::SystemFraming];

    pub(crate) fn artifact(self) -> PromptArtifact {
        match self {
            Self::SystemFraming => PromptArtifact {
                id: self,
                role: Role::System,
                body: SYSTEM_FRAMING_ASSET,
            },
        }
    }

    pub(crate) fn body(self) -> &'static str {
        self.artifact().body
    }
}

pub(crate) const SYSTEM_FRAMING_ASSET: &str =
    include_str!("../../prompts/harness/system-framing.md");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PromptArtifact {
    id: PromptId,
    role: Role,
    body: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_complete_nonempty_and_system_role() {
        let artifacts = PromptId::ALL.map(PromptId::artifact);
        assert_eq!(artifacts.map(|artifact| artifact.id), PromptId::ALL);
        assert!(artifacts
            .iter()
            .all(|artifact| !artifact.body.trim().is_empty()));
        assert!(artifacts
            .iter()
            .all(|artifact| artifact.role == Role::System));
    }
}
