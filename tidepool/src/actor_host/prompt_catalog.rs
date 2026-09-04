#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PromptId {
    ShoalRoot,
    RecreatedRoot,
    WorktreeAgent,
    ReadonlyAgent,
    ScaffoldingAgent,
    IntegrationAgent,
}

impl PromptId {
    #[cfg(test)]
    pub(super) const ALL: [Self; 6] = [
        Self::ShoalRoot,
        Self::RecreatedRoot,
        Self::WorktreeAgent,
        Self::ReadonlyAgent,
        Self::ScaffoldingAgent,
        Self::IntegrationAgent,
    ];

    pub(super) fn artifact(self) -> PromptArtifact {
        let body = match self {
            Self::ShoalRoot => include_str!("../../../prompts/shoal/root.md"),
            Self::RecreatedRoot => include_str!("../../../prompts/shoal/recreated-root.md"),
            Self::WorktreeAgent => include_str!("../../../prompts/shoal/worktree-agent.md"),
            Self::ReadonlyAgent => include_str!("../../../prompts/shoal/readonly-agent.md"),
            Self::ScaffoldingAgent => {
                include_str!("../../../prompts/shoal/scaffolding-agent.md")
            }
            Self::IntegrationAgent => {
                include_str!("../../../prompts/shoal/integration-agent.md")
            }
        };
        PromptArtifact {
            id: self,
            role: PromptRole::Developer,
            body,
        }
    }

    pub(super) fn body(self) -> &'static str {
        self.artifact().body
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PromptRole {
    Developer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PromptArtifact {
    pub(super) id: PromptId,
    pub(super) role: PromptRole,
    pub(super) body: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_complete_nonempty_and_developer_role() {
        let artifacts = PromptId::ALL.map(PromptId::artifact);
        assert_eq!(artifacts.map(|artifact| artifact.id), PromptId::ALL);
        assert!(artifacts
            .iter()
            .all(|artifact| !artifact.body.trim().is_empty()));
        assert!(artifacts
            .iter()
            .all(|artifact| artifact.role == PromptRole::Developer));
    }
}
