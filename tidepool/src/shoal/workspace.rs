//! Workspace-authored inputs selected once for an entire Shoal run.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct HaskellConfig {
    pub source_roots: Vec<PathBuf>,
    pub modules: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct PromptConfig {
    pub core: Option<PathBuf>,
    pub root: Option<PathBuf>,
    pub research: Option<PathBuf>,
    pub coding: Option<PathBuf>,
    pub scaffolding: Option<PathBuf>,
    pub integration: Option<PathBuf>,
}

/// Captured workspace inputs. Actor launch and compilation use only this run's
/// materialized paths and bytes, never the mutable workspace configuration.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FrozenWorkspace {
    pub(crate) include: Vec<PathBuf>,
    pub(crate) modules: Vec<String>,
    pub(crate) prompts: BTreeMap<String, String>,
    files: BTreeMap<PathBuf, String>,
    config: String,
    library_identity: String,
}

impl FrozenWorkspace {
    pub(crate) fn load(workspace: &Path, run_root: &Path) -> Result<Self> {
        let directory = run_root.join("workspace");
        let manifest = directory.join("selection.json");
        if manifest.exists() {
            let frozen: Self = serde_json::from_slice(&std::fs::read(&manifest)?)?;
            if frozen.library_identity != crate::haskell_sources::source_identity() {
                return Err(
                    "frozen workspace library differs from this build; start a new swarm".into(),
                );
            }
            for (relative, hash) in &frozen.files {
                let bytes = std::fs::read(directory.join(relative))?;
                if blake3::hash(&bytes).to_hex().as_str() != hash {
                    return Err(
                        format!("frozen workspace input changed: {}", relative.display()).into(),
                    );
                }
            }
            return Ok(frozen);
        }
        let (config, config_text) = super::read_project_config(workspace)?;
        let base = workspace.join(".shoal");
        std::fs::create_dir_all(&directory)?;
        let mut files = BTreeMap::new();
        let mut include = Vec::new();
        // Failed captures have no manifest. A retry selects fresh directories,
        // so deleted modules from a partial attempt cannot remain importable.
        let capture = uuid::Uuid::new_v4();
        for (index, root) in config.haskell.source_roots.iter().enumerate() {
            let source = base.join(root).canonicalize()?;
            let relative = PathBuf::from(format!("sources/{capture}/{index}"));
            capture_sources(&source, &relative, &directory, &mut files)?;
            include.push(directory.join(relative));
        }
        for module in &config.haskell.modules {
            if !valid_module(module) {
                return Err(format!("invalid configured Haskell module: {module:?}").into());
            }
            let relative = module.replace('.', "/");
            if !include.iter().any(|root| {
                root.join(format!("{relative}.hs")).is_file()
                    || root.join(format!("{relative}.lhs")).is_file()
            }) {
                return Err(format!(
                    "configured module {module} is missing from captured source roots"
                )
                .into());
            }
        }
        let mut prompts = BTreeMap::new();
        for (name, path) in [
            ("core", config.prompts.core),
            ("root", config.prompts.root),
            ("research", config.prompts.research),
            ("coding", config.prompts.coding),
            ("scaffolding", config.prompts.scaffolding),
            ("integration", config.prompts.integration),
        ] {
            if let Some(path) = path {
                let text = std::fs::read_to_string(base.join(path))?;
                if text.trim().is_empty() {
                    return Err(format!("configured {name} prompt is empty").into());
                }
                prompts.insert(name.to_owned(), text);
            }
        }
        let frozen = Self {
            include,
            modules: config.haskell.modules,
            prompts,
            files,
            config: config_text,
            library_identity: crate::haskell_sources::source_identity(),
        };
        tidepool_atomic_write::write_durable(&manifest, &serde_json::to_vec_pretty(&frozen)?)?;
        Ok(frozen)
    }

    pub(crate) fn imports(&self) -> Vec<String> {
        self.modules
            .iter()
            .map(|module| format!("import {module}"))
            .collect()
    }

    pub(super) fn config(&self) -> Result<super::ShoalConfig> {
        Ok(toml::from_str(&self.config)?)
    }
}

fn valid_module(module: &str) -> bool {
    !module.is_empty()
        && module.split('.').all(|part| {
            let mut chars = part.chars();
            chars.next().is_some_and(|c| c.is_ascii_uppercase())
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '\'')
        })
}

fn capture_sources(
    source: &Path,
    relative: &Path,
    destination: &Path,
    files: &mut BTreeMap<PathBuf, String>,
) -> Result<()> {
    std::fs::create_dir_all(destination.join(relative))?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        // Source roots may be `.shoal` itself. Runtime/build trees are not inputs.
        if matches!(
            name.to_str(),
            Some("logs" | "sessions" | "runtime" | "build" | ".git" | "dist-newstyle" | "target")
        ) {
            continue;
        }
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err(format!(
                "configured source tree contains a symlink: {}",
                entry.path().display()
            )
            .into());
        }
        let path = relative.join(&name);
        if kind.is_dir() {
            capture_sources(&entry.path(), &path, destination, files)?;
        } else if matches!(
            entry.path().extension().and_then(|x| x.to_str()),
            Some("hs" | "lhs" | "hs-boot" | "h")
        ) {
            let bytes = std::fs::read(entry.path())?;
            tidepool_atomic_write::write_durable(&destination.join(&path), &bytes)?;
            files.insert(path, blake3::hash(&bytes).to_hex().to_string());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_freezes_dependencies_prompts_and_configuration_until_next_run() {
        let project = tempfile::tempdir().unwrap();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let authored = project.path().join(".shoal");
        std::fs::create_dir_all(authored.join("Project")).unwrap();
        std::fs::write(authored.join("config.toml"), "[defaults]\nmodel = 'gpt-5.6-sol'\n[haskell]\nsource_roots = ['.']\nmodules = ['Project.Work']\n[prompts]\ncore = 'core.md'\n").unwrap();
        std::fs::write(
            authored.join("Project/Work.hs"),
            "module Project.Work where\nimport Project.Types\n",
        )
        .unwrap();
        std::fs::write(
            authored.join("Project/Types.hs"),
            "module Project.Types where\ndata Result = Old\n",
        )
        .unwrap();
        std::fs::write(authored.join("core.md"), "Original guidance").unwrap();
        let frozen = FrozenWorkspace::load(project.path(), first.path()).unwrap();
        std::fs::write(
            authored.join("Project/Types.hs"),
            "module Project.Types where\ndata Result = New\n",
        )
        .unwrap();
        std::fs::write(authored.join("core.md"), "Revised guidance").unwrap();
        let reused = FrozenWorkspace::load(project.path(), first.path()).unwrap();
        assert_eq!(reused.prompts, frozen.prompts);
        assert!(
            std::fs::read_to_string(reused.include[0].join("Project/Types.hs"))
                .unwrap()
                .contains("Old")
        );
        let next = FrozenWorkspace::load(project.path(), second.path()).unwrap();
        assert_eq!(next.prompts["core"], "Revised guidance");
        assert!(
            std::fs::read_to_string(next.include[0].join("Project/Types.hs"))
                .unwrap()
                .contains("New")
        );
        std::fs::write(frozen.include[0].join("Project/Types.hs"), "tampered").unwrap();
        assert!(FrozenWorkspace::load(project.path(), first.path())
            .unwrap_err()
            .to_string()
            .contains("frozen workspace input changed"));
    }

    #[test]
    fn invalid_import_or_missing_source_fails_before_launch() {
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir(project.path().join(".shoal")).unwrap();
        for module in ["Project.Work\nimport Bad", "Project.Missing"] {
            let run = tempfile::tempdir().unwrap();
            std::fs::write(
                project.path().join(".shoal/config.toml"),
                format!(
                    "[defaults]\nmodel = 'gpt-5.6-sol'\n[haskell]\nmodules = [{}]\n",
                    serde_json::to_string(module).unwrap()
                ),
            )
            .unwrap();
            assert!(FrozenWorkspace::load(project.path(), run.path()).is_err());
        }
    }
}
