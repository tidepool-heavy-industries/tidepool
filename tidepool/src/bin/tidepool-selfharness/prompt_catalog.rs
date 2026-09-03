#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PromptId {
    MemoryCurator,
}

impl PromptId {
    #[cfg(test)]
    const ALL: [Self; 1] = [Self::MemoryCurator];

    pub(super) fn artifact(self) -> PromptArtifact {
        match self {
            Self::MemoryCurator => PromptArtifact {
                id: self,
                role: PromptRole::RepositoryDeveloperPolicy,
                body: include_str!("../../../../prompts/selfharness/memory-curator.md"),
            },
        }
    }

    pub(super) fn body(self) -> &'static str {
        self.artifact().body
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PromptRole {
    RepositoryDeveloperPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PromptArtifact {
    id: PromptId,
    role: PromptRole,
    body: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_complete_nonempty_and_repository_policy_role() {
        let artifacts = PromptId::ALL.map(PromptId::artifact);
        assert_eq!(artifacts.map(|artifact| artifact.id), PromptId::ALL);
        assert!(artifacts
            .iter()
            .all(|artifact| !artifact.body.trim().is_empty()));
        assert!(artifacts
            .iter()
            .all(|artifact| artifact.role == PromptRole::RepositoryDeveloperPolicy));
    }
}
