#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PromptId {
    TreePractice,
    ShoalRoot,
    RecreatedRoot,
    WorktreeAgent,
    ReadonlyAgent,
    ScaffoldingAgent,
    IntegrationAgent,
}

impl PromptId {
    pub(super) const CATALOG_VERSION: u32 = 8;

    #[cfg(test)]
    pub(super) const ALL: [Self; 7] = [
        Self::TreePractice,
        Self::ShoalRoot,
        Self::RecreatedRoot,
        Self::WorktreeAgent,
        Self::ReadonlyAgent,
        Self::ScaffoldingAgent,
        Self::IntegrationAgent,
    ];

    pub(super) fn artifact(self) -> PromptArtifact {
        let body = match self {
            Self::TreePractice => include_str!("../../../prompts/shoal/tree-practice.md"),
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
            catalog_version: Self::CATALOG_VERSION,
            body,
        }
    }

    pub(super) fn body(self) -> &'static str {
        self.artifact().body
    }

    pub(super) fn composed_fingerprint(body: &str, hosted_tool_fingerprint: &str) -> String {
        let mut hasher = blake3::Hasher::new();
        for part in [body, hosted_tool_fingerprint] {
            hasher.update(&(part.len() as u64).to_le_bytes());
            hasher.update(part.as_bytes());
        }
        hasher.finalize().to_hex().to_string()
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
    pub(super) catalog_version: u32,
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
        assert!(artifacts
            .iter()
            .all(|artifact| artifact.catalog_version == PromptId::CATALOG_VERSION));
        assert_eq!(
            PromptId::composed_fingerprint(PromptId::ShoalRoot.body(), "hosted-tool-fingerprint")
                .len(),
            64
        );
    }
}
