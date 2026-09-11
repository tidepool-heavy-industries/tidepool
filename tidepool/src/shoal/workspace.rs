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
    pub checks: Vec<String>,
    pub tools: Option<String>,
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
    pub files: BTreeMap<String, PathBuf>,
}

/// Captured workspace inputs. Actor launch and compilation use only this run's
/// materialized paths and bytes, never the mutable workspace configuration.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FrozenWorkspace {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    identity: String,
    pub(crate) include: Vec<PathBuf>,
    pub(crate) modules: Vec<String>,
    #[serde(default)]
    pub(crate) checks: Vec<String>,
    #[serde(default)]
    pub(crate) tools: Option<String>,
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
            if frozen.version != 1 {
                return Err("unsupported frozen workspace format; start a new swarm".into());
            }
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
        for entry in config
            .haskell
            .checks
            .iter()
            .chain(config.haskell.tools.iter())
        {
            let Some((module, function)) = entry.rsplit_once('.') else {
                return Err(format!("Haskell entry must be Module.function: {entry}").into());
            };
            if !valid_module(module)
                || !function.starts_with(|c: char| c.is_ascii_lowercase())
                || !function
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '\'')
            {
                return Err(format!("invalid Haskell entry: {entry}").into());
            }
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
        for (name, path) in config.prompts.files {
            if name.trim().is_empty()
                || matches!(
                    name.as_str(),
                    "core" | "root" | "research" | "coding" | "scaffolding" | "integration"
                )
            {
                return Err(format!("project prompt name is empty or reserved: {name:?}").into());
            }
            let text = std::fs::read_to_string(base.join(path))?;
            if text.trim().is_empty() {
                return Err(format!("configured project prompt {name:?} is empty").into());
            }
            prompts.insert(name, text);
        }
        let library_identity = crate::haskell_sources::source_identity();
        let source_prefix = PathBuf::from(format!("sources/{capture}"));
        let logical_files = files
            .iter()
            .map(|(path, hash)| (path.strip_prefix(&source_prefix).unwrap_or(path), hash))
            .collect::<Vec<_>>();
        let identity = blake3::hash(&serde_json::to_vec(&(
            &config_text,
            &library_identity,
            &prompts,
            logical_files,
        ))?)
        .to_hex()
        .to_string();
        let resources = resource_module(&identity, &config.haskell.modules, &prompts);
        let resources_path = PathBuf::from("resources/Shoal/Workspace.hs");
        if include.iter().any(|root| {
            root.join("Shoal/Workspace.hs").exists() || root.join("Shoal/Workspace.lhs").exists()
        }) {
            return Err("Shoal.Workspace is reserved for the frozen workspace interface".into());
        }
        std::fs::create_dir_all(directory.join("resources/Shoal"))?;
        tidepool_atomic_write::write_durable(
            &directory.join(&resources_path),
            resources.as_bytes(),
        )?;
        files.insert(
            resources_path,
            blake3::hash(resources.as_bytes()).to_hex().to_string(),
        );
        include.push(directory.join("resources"));
        let frozen = Self {
            version: 1,
            identity,
            include,
            modules: config.haskell.modules,
            checks: config.haskell.checks,
            tools: config.haskell.tools,
            prompts,
            files,
            config: config_text,
            library_identity,
        };
        tidepool_atomic_write::write_durable(&manifest, &serde_json::to_vec_pretty(&frozen)?)?;
        Ok(frozen)
    }

    pub(crate) fn identity(&self) -> &str {
        &self.identity
    }

    pub(crate) fn imports(&self) -> Vec<String> {
        self.import_modules()
            .map(|module| format!("import {module}"))
            .collect()
    }

    pub(crate) fn import_modules(&self) -> impl Iterator<Item = &str> {
        std::iter::once("Shoal.Workspace").chain(self.modules.iter().map(String::as_str))
    }

    pub(crate) fn config(&self) -> Result<super::ShoalConfig> {
        Ok(toml::from_str(&self.config)?)
    }
}

fn resource_module(
    identity: &str,
    modules: &[String],
    prompts: &BTreeMap<String, String>,
) -> String {
    let literal = |value: &str| {
        format!(
            "\"{}\"",
            tidepool_runtime::session::escape_workbench_haskell_string(value)
        )
    };
    let module_names = modules
        .iter()
        .map(|name| literal(name))
        .collect::<Vec<_>>()
        .join(", ");
    let entries = prompts
        .iter()
        .map(|(name, body)| format!("({}, {})", literal(name), literal(body)))
        .collect::<Vec<_>>()
        .join(",\n  ");
    format!(
        "{}\nworkspaceIdentity = {}\nworkspaceModules = [{}]\nworkspacePrompts = [{}]\n",
        include_str!("workspace.hs"),
        literal(identity),
        module_names,
        entries
    )
}

fn valid_module(module: &str) -> bool {
    !module.is_empty()
        && module.split('.').all(|part| {
            let mut chars = part.chars();
            chars.next().is_some_and(|c| c.is_ascii_uppercase())
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '\'')
        })
}

/// Copy the authored package into an isolated check repository, excluding runtime trees.
pub(crate) fn copy_authored(workspace: &Path, destination: &Path) -> Result<()> {
    capture_tree(
        &workspace.join(".shoal"),
        Path::new(".shoal"),
        destination,
        &mut BTreeMap::new(),
        true,
    )
}

fn capture_sources(
    source: &Path,
    relative: &Path,
    destination: &Path,
    files: &mut BTreeMap<PathBuf, String>,
) -> Result<()> {
    capture_tree(source, relative, destination, files, false)
}

fn capture_tree(
    source: &Path,
    relative: &Path,
    destination: &Path,
    files: &mut BTreeMap<PathBuf, String>,
    all_authored: bool,
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
            capture_tree(&entry.path(), &path, destination, files, all_authored)?;
        } else if all_authored
            || matches!(
                entry.path().extension().and_then(|x| x.to_str()),
                Some("hs" | "lhs" | "hs-boot" | "h")
            )
        {
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
        let identical_run = tempfile::tempdir().unwrap();
        let identical = FrozenWorkspace::load(project.path(), identical_run.path()).unwrap();
        assert_eq!(identical.identity, frozen.identity);
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
        assert_ne!(next.identity, frozen.identity);
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

    #[test]
    fn tool_selection_is_qualified_and_frozen_with_the_package() {
        let project = tempfile::tempdir().unwrap();
        let authored = project.path().join(".shoal");
        std::fs::create_dir(&authored).unwrap();
        let write = |entry: &str| {
            std::fs::write(
                authored.join("config.toml"),
                format!(
                    "[defaults]\nmodel='gpt-5.6-sol'\n[haskell]\ntools={}\n",
                    serde_json::to_string(entry).unwrap(),
                ),
            )
            .unwrap()
        };
        for entry in [
            "tools",
            "Project.Tools.tools\nimport Bad",
            "Project.Tools.Tools",
            "Project.Tools.tools ()",
        ] {
            write(entry);
            assert!(
                FrozenWorkspace::load(project.path(), tempfile::tempdir().unwrap().path()).is_err()
            );
        }
        write("Tidepool.Command.Tools.tools");
        let run = tempfile::tempdir().unwrap();
        let frozen = FrozenWorkspace::load(project.path(), run.path()).unwrap();
        assert_eq!(
            frozen.tools.as_deref(),
            Some("Tidepool.Command.Tools.tools")
        );
        write("Project.Next.tools");
        let same_run = FrozenWorkspace::load(project.path(), run.path()).unwrap();
        assert_eq!(same_run.tools, frozen.tools);
        let next =
            FrozenWorkspace::load(project.path(), tempfile::tempdir().unwrap().path()).unwrap();
        assert_eq!(next.tools.as_deref(), Some("Project.Next.tools"));
        assert_ne!(next.identity, frozen.identity);
    }

    #[test]
    fn workspace_program_validation_uses_frozen_sources_and_rejects_bad_revisions() {
        let project = tempfile::tempdir().unwrap();
        let old_run = tempfile::tempdir().unwrap();
        let new_run = tempfile::tempdir().unwrap();
        let authored = project.path().join(".shoal");
        std::fs::create_dir_all(authored.join("Project")).unwrap();
        std::fs::write(
            authored.join("config.toml"),
            "[defaults]\nmodel = 'gpt-5.6-sol'\n[haskell]\nsource_roots = ['.']\nmodules = ['Project.Check']\n",
        )
        .unwrap();
        let source = authored.join("Project/Check.hs");
        std::fs::write(
            &source,
            "module Project.Check where\ncheck :: Bool\ncheck = True\n",
        )
        .unwrap();
        let old = FrozenWorkspace::load(project.path(), old_run.path()).unwrap();
        std::fs::write(
            &source,
            "module Project.Check where\ncheck :: Bool\ncheck = undefinedWorkspaceFunction\n",
        )
        .unwrap();
        crate::actor_host::validate_workspace_program(&old, old_run.path()).unwrap();
        let new = FrozenWorkspace::load(project.path(), new_run.path()).unwrap();
        let error =
            crate::actor_host::validate_workspace_program(&new, new_run.path()).unwrap_err();
        assert!(
            error.to_string().contains("undefinedWorkspaceFunction"),
            "{error}"
        );
    }
}
