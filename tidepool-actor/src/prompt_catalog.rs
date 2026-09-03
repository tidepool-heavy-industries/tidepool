#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromptId {
    HaskellToolDescription,
    HaskellToolInstructions,
}

impl PromptId {
    #[cfg(test)]
    pub(crate) const ALL: [Self; 2] = [Self::HaskellToolDescription, Self::HaskellToolInstructions];

    pub(crate) fn artifact(self) -> PromptArtifact {
        match self {
            Self::HaskellToolDescription => PromptArtifact {
                id: self,
                role: PromptRole::HostedToolDescription,
                body: include_str!("../../prompts/shoal/haskell-tool-description.md"),
            },
            Self::HaskellToolInstructions => PromptArtifact {
                id: self,
                role: PromptRole::HostedToolInstructions,
                body: include_str!("../../prompts/shoal/haskell-tool-instructions.md"),
            },
        }
    }

    pub(crate) fn body(self) -> &'static str {
        self.artifact().body
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromptRole {
    HostedToolDescription,
    HostedToolInstructions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PromptArtifact {
    pub(crate) id: PromptId,
    pub(crate) role: PromptRole,
    pub(crate) body: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_complete_nonempty_and_role_typed() {
        let artifacts = PromptId::ALL.map(PromptId::artifact);
        assert_eq!(artifacts.map(|artifact| artifact.id), PromptId::ALL);
        assert!(artifacts
            .iter()
            .all(|artifact| !artifact.body.trim().is_empty()));
        assert_eq!(
            artifacts.map(|artifact| artifact.role),
            [
                PromptRole::HostedToolDescription,
                PromptRole::HostedToolInstructions,
            ]
        );
        let description = PromptId::HaskellToolDescription.body();
        assert!(description.contains(":type complete"));
        assert!(!description.contains("complete action"));
        assert!(!description.contains("assemble"));
    }
}
