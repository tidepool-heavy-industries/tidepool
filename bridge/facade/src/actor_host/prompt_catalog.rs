#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PromptId {
    ExomonadBase,
    ExomonadRoot,
    RecreatedRoot,
    WorktreeAgent,
    ReadonlyAgent,
    ScaffoldingAgent,
    IntegrationAgent,
}

impl PromptId {
    pub(super) const CATALOG_VERSION: u32 = 34;

    /// Digest of every prompt body in [`PromptId::ALL`] order — the guard
    /// `catalog_body_fingerprint_matches_prompt_bodies` fails loudly, naming
    /// the correct new value, whenever a prompt body changes without a
    /// matching `CATALOG_VERSION` bump.
    #[cfg(test)]
    pub(super) const CATALOG_BODY_FINGERPRINT: &'static str =
        "73d8b532a5b7332e2daf928ef881ee89e3f2d0fcf2ab8169e548bf71ee4ccc74";

    #[cfg(test)]
    pub(super) const ALL: [Self; 7] = [
        Self::ExomonadBase,
        Self::ExomonadRoot,
        Self::RecreatedRoot,
        Self::WorktreeAgent,
        Self::ReadonlyAgent,
        Self::ScaffoldingAgent,
        Self::IntegrationAgent,
    ];

    pub(super) fn artifact(self) -> PromptArtifact {
        let body = match self {
            Self::ExomonadBase => concat!(
                include_str!("../../../../exomonad/prompts/base.md"),
                "\n\n",
                include_str!("../../../../exomonad/prompts/api-guide.md")
            ),
            Self::ExomonadRoot => include_str!("../../../../exomonad/prompts/root.md"),
            Self::RecreatedRoot => include_str!("../../../../exomonad/prompts/recreated-root.md"),
            Self::WorktreeAgent => include_str!("../../../../exomonad/prompts/worktree-agent.md"),
            Self::ReadonlyAgent => include_str!("../../../../exomonad/prompts/readonly-agent.md"),
            Self::ScaffoldingAgent => {
                include_str!("../../../../exomonad/prompts/scaffolding-agent.md")
            }
            Self::IntegrationAgent => {
                include_str!("../../../../exomonad/prompts/integration-agent.md")
            }
        };
        PromptArtifact {
            id: self,
            role: if self == Self::ExomonadBase {
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
        tidepool_toolchain::digest::of_parts(
            [base, body, hosted_tool_fingerprint].map(str::as_bytes),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PromptRole {
    BaseInstructions,
    Developer,
}

/// Whether this run's workspace supplies the Jev authoring surface.
///
/// The base instructions describe Jev unconditionally, because it is the
/// surface a workspace is expected to pin. A workspace that has not pinned it
/// gets one line saying so, so the agent does not spend a turn discovering
/// that `J` is not in its workbench.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum JevSurface {
    Installed,
    Absent,
}

const JEV_ABSENT: &str = "\n\n\
# Jev is not installed in this workspace\n\n\
This workspace supplies no `Jev.Operators`, so `J` is absent from your workbench \
and the Jev guidance above does not apply here. Installing it is two lines: the \
`jev-dsl` input in the project's `flake.nix`, and `jev-dsl = [\"core\"]` under \
`[haskell.flake_sources]` in `.exomonad/config.toml`. `exomonad new` writes both; a \
project that has them already needs `nix flake lock` and a new run.\n";

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
        Self::materialize_selected(run_root, None, JevSurface::Installed)
    }

    pub(super) fn materialize_selected(
        run_root: &std::path::Path,
        core: Option<&str>,
        jev: JevSurface,
    ) -> std::io::Result<Self> {
        let body = Self::selected_body(core, jev);
        let directory = run_root.join("prompts");
        std::fs::create_dir_all(&directory)?;
        let directory = directory.canonicalize()?;
        let file = directory.join(format!("{}.md", blake3::hash(body.as_bytes()).to_hex()));
        match std::fs::read(&file) {
            Ok(existing) if existing == body.as_bytes() => {}
            Ok(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "saved Exomonad base prompt does not match its content identity",
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

    pub(super) fn selected_body(core: Option<&str>, jev: JevSurface) -> String {
        let mut body = match core {
            Some(core) => format!(
                "{core}\n\n{}",
                include_str!("../../../../exomonad/prompts/api-guide.md")
            ),
            None => PromptId::ExomonadBase.body().to_owned(),
        };
        if jev == JevSurface::Absent {
            body.push_str(JEV_ABSENT);
        }
        body
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
    fn shared_prompt_stays_within_word_budget() {
        let words = PromptId::ExomonadBase.body().split_whitespace().count();
        assert!(
            words <= 3000,
            "shared base/API has {words} words; budget is 3000"
        );
    }

    #[test]
    fn frozen_base_reuses_exact_bytes_and_rejects_changed_artifacts() {
        let root = tempfile::tempdir().unwrap();
        let first = FrozenBasePrompt::materialize(root.path()).unwrap();
        let second = FrozenBasePrompt::materialize(root.path()).unwrap();
        assert_eq!(first.file(), second.file());
        assert_eq!(
            std::fs::read_to_string(first.file()).unwrap(),
            PromptId::ExomonadBase.body()
        );
        std::fs::write(first.file(), "unexpected instructions").unwrap();
        assert!(
            matches!(FrozenBasePrompt::materialize(root.path()), Err(error) if error.kind() == std::io::ErrorKind::InvalidData)
        );
        let blocked = tempfile::NamedTempFile::new().unwrap();
        assert!(FrozenBasePrompt::materialize(blocked.path()).is_err());
    }

    /// The base instructions describe Jev unconditionally. A workspace that
    /// does not supply it says so once, in the same instructions, rather than
    /// letting the agent find out from a cell that will not compile.
    #[test]
    fn a_workspace_without_jev_says_so_in_the_instructions() {
        let installed = FrozenBasePrompt::selected_body(None, JevSurface::Installed);
        let absent = FrozenBasePrompt::selected_body(None, JevSurface::Absent);
        assert_eq!(installed, PromptId::ExomonadBase.body());
        assert!(absent.starts_with(&installed));
        assert!(
            absent.contains("Jev is not installed in this workspace"),
            "{absent}"
        );
        assert!(absent.contains("[haskell.flake_sources]"), "{absent}");
        assert!(absent.contains("exomonad new"), "{absent}");
        let selected = FrozenBasePrompt::selected_body(Some("project core"), JevSurface::Absent);
        assert!(selected.starts_with("project core"));
        assert!(selected.contains("Jev is not installed in this workspace"));
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
    fn catalog_body_fingerprint_matches_prompt_bodies() {
        let bodies = PromptId::ALL.map(PromptId::body);
        let computed = tidepool_toolchain::digest::of_parts(bodies.map(str::as_bytes));
        assert_eq!(
            computed,
            PromptId::CATALOG_BODY_FINGERPRINT,
            "prompt bodies changed: bump CATALOG_VERSION and update CATALOG_BODY_FINGERPRINT to {computed}"
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
            == if artifact.id == PromptId::ExomonadBase {
                PromptRole::BaseInstructions
            } else {
                PromptRole::Developer
            }));
        assert!(artifacts
            .iter()
            .all(|artifact| artifact.catalog_version == PromptId::CATALOG_VERSION));
        assert_eq!(
            PromptId::composed_fingerprint(
                PromptId::ExomonadBase.body(),
                PromptId::ExomonadRoot.body(),
                "hosted-tool-fingerprint"
            )
            .len(),
            64
        );
    }

    #[test]
    fn shared_api_guide_is_part_of_the_frozen_base_not_role_instructions() {
        let guide = include_str!("../../../../exomonad/prompts/api-guide.md");
        let base = PromptId::ExomonadBase.body();
        assert_eq!(
            base,
            format!(
                "{}\n\n{guide}",
                include_str!("../../../../exomonad/prompts/base.md")
            )
        );
        for id in PromptId::ALL {
            if id != PromptId::ExomonadBase {
                assert!(!id.body().contains(guide));
            }
        }
        let root = tempfile::tempdir().unwrap();
        let frozen = FrozenBasePrompt::materialize(root.path()).unwrap();
        assert_eq!(std::fs::read_to_string(frozen.file()).unwrap(), base);
    }
}
