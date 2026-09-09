#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PromptId {
    ShoalBase,
    ShoalRoot,
    RecreatedRoot,
    WorktreeAgent,
    ReadonlyAgent,
    ScaffoldingAgent,
    IntegrationAgent,
}

impl PromptId {
    pub(super) const CATALOG_VERSION: u32 = 23;

    #[cfg(test)]
    pub(super) const ALL: [Self; 7] = [
        Self::ShoalBase,
        Self::ShoalRoot,
        Self::RecreatedRoot,
        Self::WorktreeAgent,
        Self::ReadonlyAgent,
        Self::ScaffoldingAgent,
        Self::IntegrationAgent,
    ];

    pub(super) fn artifact(self) -> PromptArtifact {
        let body = match self {
            Self::ShoalBase => concat!(
                include_str!("../../../prompts/shoal/base.md"),
                "\n\n",
                include_str!("../../../prompts/shoal/api-guide.md")
            ),
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
            role: if self == Self::ShoalBase {
                PromptRole::BaseInstructions
            } else {
                PromptRole::Developer
            },
            catalog_version: Self::CATALOG_VERSION,
            body,
        }
    }

    pub(super) fn body(self) -> &'static str {
        self.artifact().body
    }

    pub(super) fn composed_fingerprint(
        base: &str,
        body: &str,
        hosted_tool_fingerprint: &str,
    ) -> String {
        let mut hasher = blake3::Hasher::new();
        for part in [base, body, hosted_tool_fingerprint] {
            hasher.update(&(part.len() as u64).to_le_bytes());
            hasher.update(part.as_bytes());
        }
        hasher.finalize().to_hex().to_string()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PromptRole {
    BaseInstructions,
    Developer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PromptArtifact {
    pub(super) id: PromptId,
    pub(super) role: PromptRole,
    pub(super) catalog_version: u32,
    pub(super) body: &'static str,
}

/// One run-selected base, materialized once and shared by every launch.
/// The file is content-addressed and checked on reuse. Actors receive a read-only
/// mount of its directory; launch never rereads mutable prompt source files.
#[derive(Clone)]
pub(super) struct FrozenBasePrompt {
    directory: std::path::PathBuf,
    file: std::path::PathBuf,
    body: String,
}

impl FrozenBasePrompt {
    #[cfg(test)]
    pub(super) fn materialize(run_root: &std::path::Path) -> std::io::Result<Self> {
        Self::materialize_selected(run_root, None)
    }

    pub(super) fn materialize_selected(
        run_root: &std::path::Path,
        core: Option<&str>,
    ) -> std::io::Result<Self> {
        let body = Self::selected_body(core);
        let directory = run_root.join("prompts");
        std::fs::create_dir_all(&directory)?;
        let directory = directory.canonicalize()?;
        let file = directory.join(format!("{}.md", blake3::hash(body.as_bytes()).to_hex()));
        match std::fs::read(&file) {
            Ok(existing) if existing == body.as_bytes() => {}
            Ok(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "saved Shoal base prompt does not match its content identity",
                ))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                tidepool_atomic_write::write_durable(&file, body.as_bytes())?;
            }
            Err(error) => return Err(error),
        }
        Ok(Self {
            directory,
            file,
            body,
        })
    }

    pub(super) fn selected_body(core: Option<&str>) -> String {
        match core {
            Some(core) => format!(
                "{core}\n\n{}",
                include_str!("../../../prompts/shoal/api-guide.md")
            ),
            None => PromptId::ShoalBase.body().to_owned(),
        }
    }

    pub(super) fn body(&self) -> &str {
        &self.body
    }

    pub(super) fn directory(&self) -> &std::path::Path {
        &self.directory
    }
    pub(super) fn file(&self) -> &std::path::Path {
        &self.file
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frozen_base_reuses_exact_bytes_and_rejects_changed_artifacts() {
        let root = tempfile::tempdir().unwrap();
        let first = FrozenBasePrompt::materialize(root.path()).unwrap();
        let second = FrozenBasePrompt::materialize(root.path()).unwrap();
        assert_eq!(first.file(), second.file());
        assert_eq!(
            std::fs::read_to_string(first.file()).unwrap(),
            PromptId::ShoalBase.body()
        );
        std::fs::write(first.file(), "unexpected instructions").unwrap();
        assert!(
            matches!(FrozenBasePrompt::materialize(root.path()), Err(error) if error.kind() == std::io::ErrorKind::InvalidData)
        );
        let blocked = tempfile::NamedTempFile::new().unwrap();
        assert!(FrozenBasePrompt::materialize(blocked.path()).is_err());
    }

    #[test]
    fn composed_identity_covers_base_role_and_tools_separately() {
        let original = PromptId::composed_fingerprint("base", "role", "tools");
        for parts in [
            ("changed", "role", "tools"),
            ("base", "changed", "tools"),
            ("base", "role", "changed"),
        ] {
            assert_ne!(
                original,
                PromptId::composed_fingerprint(parts.0, parts.1, parts.2)
            );
        }
        assert_ne!(
            PromptId::composed_fingerprint("ab", "c", "d"),
            PromptId::composed_fingerprint("a", "bc", "d")
        );
    }

    #[test]
    fn catalog_separates_base_from_role_instructions() {
        let artifacts = PromptId::ALL.map(PromptId::artifact);
        assert_eq!(artifacts.map(|artifact| artifact.id), PromptId::ALL);
        assert!(artifacts
            .iter()
            .all(|artifact| !artifact.body.trim().is_empty()));
        assert!(artifacts.iter().all(|artifact| artifact.role
            == if artifact.id == PromptId::ShoalBase {
                PromptRole::BaseInstructions
            } else {
                PromptRole::Developer
            }));
        assert!(artifacts
            .iter()
            .all(|artifact| artifact.catalog_version == PromptId::CATALOG_VERSION));
        assert_eq!(
            PromptId::composed_fingerprint(
                PromptId::ShoalBase.body(),
                PromptId::ShoalRoot.body(),
                "hosted-tool-fingerprint"
            )
            .len(),
            64
        );
    }

    #[test]
    fn shared_api_guide_is_part_of_the_frozen_base_not_role_instructions() {
        let guide = include_str!("../../../prompts/shoal/api-guide.md");
        let base = PromptId::ShoalBase.body();
        assert_eq!(
            base,
            format!(
                "{}\n\n{guide}",
                include_str!("../../../prompts/shoal/base.md")
            )
        );
        for id in PromptId::ALL {
            if id != PromptId::ShoalBase {
                assert!(!id.body().contains(guide));
            }
        }
        let root = tempfile::tempdir().unwrap();
        let frozen = FrozenBasePrompt::materialize(root.path()).unwrap();
        assert_eq!(std::fs::read_to_string(frozen.file()).unwrap(), base);
    }
}
