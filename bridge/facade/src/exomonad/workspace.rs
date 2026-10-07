//! Workspace-authored inputs selected once for an entire Exomonad run.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tidepool_toolchain::toolchain::{NativeCatalogSourceSelection, NativeSourceRole};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct HaskellConfig {
    pub source_roots: Vec<PathBuf>,
    /// Haskell source directories inside the project's flake inputs, keyed by
    /// the input name `flake.nix` declares. Each directory is relative to the
    /// root of the fetched input.
    pub flake_sources: BTreeMap<String, Vec<PathBuf>>,
    /// Directories that stand in for a pinned input while it is being
    /// developed. Relative to `.exomonad`, like every other configured path; an
    /// absolute path reaches a sibling checkout directly.
    pub flake_overrides: BTreeMap<String, PathBuf>,
    pub modules: Vec<String>,
    pub checks: Vec<String>,
    /// The workspace's agent spec, for a workspace that wants a name other
    /// than the `AgentSpec.agentSpec` an actor's own checkout supplies. Rule
    /// two of spec discovery.
    pub spec: Option<String>,
}

impl<'de> Deserialize<'de> for HaskellConfig {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Default, Deserialize)]
        #[serde(default, deny_unknown_fields)]
        struct Fields {
            source_roots: Vec<PathBuf>,
            flake_sources: BTreeMap<String, Vec<PathBuf>>,
            flake_overrides: BTreeMap<String, PathBuf>,
            modules: Vec<String>,
            checks: Vec<String>,
            spec: Option<String>,
            tools: Option<serde::de::IgnoredAny>,
        }
        let fields = Fields::deserialize(deserializer)?;
        if fields.tools.is_some() {
            return Err(serde::de::Error::custom(
                "[haskell] tools is obsolete; replace it with spec = 'Module.agentSpec' and define agentSpec = defaultSpec { specTools = yourTools } in that module",
            ));
        }
        Ok(Self {
            source_roots: fields.source_roots,
            flake_sources: fields.flake_sources,
            flake_overrides: fields.flake_overrides,
            modules: fields.modules,
            checks: fields.checks,
            spec: fields.spec,
        })
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct PromptConfig {
    pub core: Option<PathBuf>,
    pub root: Option<PathBuf>,
    pub research: Option<PathBuf>,
    pub coding: Option<PathBuf>,
    pub scaffolding: Option<PathBuf>,
    pub integration: Option<PathBuf>,
    pub files: BTreeMap<String, PathBuf>,
}

/// Deployed libraries retain one ordered original source selection.
/// The catalog identity authenticates its compiler and original module products;
/// this record pins the run's selection and grants no native value authority.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DeploymentSources {
    version: u32,
    sources: NativeCatalogSourceSelection,
    source_pin: String,
    producer_identity: [u8; 32],
    catalog_identity: String,
}

impl DeploymentSources {
    fn from_package(package: &tidepool_toolchain::toolchain::DeploymentModulePackage) -> Self {
        Self {
            version: 1,
            sources: package.source_selection().clone(),
            source_pin: package.source_identity().to_owned(),
            producer_identity: *package.producer_identity(),
            catalog_identity: package.catalog_identity().to_owned(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum RuntimeLibraries {
    Captured { stdlib: PathBuf, actors: PathBuf },
    Deployment { selection: DeploymentSources },
}

impl Default for RuntimeLibraries {
    fn default() -> Self {
        Self::Captured {
            stdlib: PathBuf::new(),
            actors: PathBuf::new(),
        }
    }
}

impl RuntimeLibraries {
    fn stdlib(&self) -> PathBuf {
        match self {
            Self::Captured { stdlib, .. } => stdlib.clone(),
            Self::Deployment { selection } => selection.sources.root(NativeSourceRole::Stdlib),
        }
    }

    fn actors(&self) -> PathBuf {
        match self {
            Self::Captured { actors, .. } => actors.clone(),
            Self::Deployment { selection } => selection.sources.root(NativeSourceRole::Actors),
        }
    }

    fn roots(&self) -> [PathBuf; 2] {
        [self.stdlib(), self.actors()]
    }

    fn deployment(&self) -> Option<&DeploymentSources> {
        match self {
            Self::Captured { .. } => None,
            Self::Deployment { selection } => Some(selection),
        }
    }
}

/// Frozen workspace inputs. Actor launch and compilation use this run's captured
/// authored sources and its pinned library selection.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum WorkspacePreparation {
    /// Persisted before compilation. Retrying the same deployment keeps the
    /// exact original namespace even after completed-output publication fails.
    Preparing { original: uuid::Uuid },
    Completed {
        original: uuid::Uuid,
        revision: String,
        coverage: Vec<PreparedToolsetCoverage>,
    },
}

/// The required profile and the exact specialization its producer completed.
/// These records are the primary inventory; recipe selections are derived.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PreparedToolsetCoverage {
    pub(crate) profile: super::PreparationProfile,
    pub(crate) requested_effects: Vec<exomonad_actor::ActorEffectKey>,
    pub(crate) effective_effects: Vec<exomonad_actor::ActorEffectKey>,
    pub(crate) recipe: String,
    pub(crate) original: uuid::Uuid,
}

fn coverage_entries(
    coverage: &[PreparedToolsetCoverage],
) -> Result<BTreeMap<String, uuid::Uuid>> {
    if coverage.is_empty() {
        return Err("completed workspace has no prepared toolset coverage".into());
    }
    let mut selected = BTreeMap::new();
    for (index, entry) in coverage.iter().enumerate() {
        if entry.recipe.len() != 64
            || !entry.recipe.bytes().all(|byte| byte.is_ascii_hexdigit())
            || entry.original.is_nil()
        {
            return Err("prepared toolset coverage has an invalid original selection".into());
        }
        if coverage[..index]
            .iter()
            .any(|previous| previous.profile == entry.profile)
        {
            return Err("prepared toolset coverage repeats a profile".into());
        }
        if let Some(previous) = coverage[..index]
            .iter()
            .find(|previous| previous.recipe == entry.recipe)
        {
            if previous.original != entry.original
                || previous.effective_effects != entry.effective_effects
            {
                return Err("prepared profiles sharing a recipe select different originals or effect rows".into());
            }
        }
        selected.insert(entry.recipe.clone(), entry.original);
    }
    Ok(selected)
}

/// A run-local reference to an independently retained immutable deployment.
/// Each run still owns its own identity, actor state and incarnation lease.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PreparedWorkspacePointer {
    pub(crate) version: u32,
    pub(crate) directory: PathBuf,
    pub(crate) selection_digest: String,
}

impl PreparedWorkspacePointer {
    pub(crate) fn for_directory(directory: &Path) -> Result<Self> {
        let directory = directory.canonicalize()?;
        let selection = std::fs::read(directory.join("workspace/selection.json"))?;
        Ok(Self {
            version: 2,
            directory,
            selection_digest: blake3::hash(&selection).to_hex().to_string(),
        })
    }

    fn read_selection(&self) -> Result<Vec<u8>> {
        if self.version != 2
            || !self.directory.is_absolute()
            || self.directory.canonicalize()? != self.directory
            || self.selection_digest.len() != 64
            || !self
                .selection_digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("prepared workspace pointer is unsupported or relocated".into());
        }
        let selection = std::fs::read(self.directory.join("workspace/selection.json"))?;
        if blake3::hash(&selection).to_hex().as_str() != self.selection_digest {
            return Err("prepared workspace selection changed after publication".into());
        }
        Ok(selection)
    }
}

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
    pub(crate) spec: Option<String>,
    pub(crate) prompts: BTreeMap<String, String>,
    #[serde(default)]
    pub(crate) models: BTreeMap<String, String>,
    files: BTreeMap<PathBuf, String>,
    config: String,
    library_identity: String,
    #[serde(default)]
    core_identity: String,
    #[serde(default)]
    runtime_libraries: RuntimeLibraries,
    /// Detect added Haskell modules as well as edits to recorded files.
    #[serde(default)]
    runtime_capture_identity: String,
    runtime_orchestration: PathBuf,
    #[serde(default)]
    pub(crate) preparation: Option<WorkspacePreparation>,
    /// Acquired only after the existing frozen-input loader validates the
    /// deployment. Deserializing a path never supplies this physical owner.
    #[serde(skip)]
    pub(crate) prepared_deployment: Option<std::sync::Arc<tidepool_atomic_write::DirectoryAnchor>>,
}

impl FrozenWorkspace {
    /// Reopen a qualified run's exact consumed deployment. Loss of this
    /// selection is a refusal; a running host cannot admit a new source capture.
    pub(crate) fn load_prepared_run(workspace: &Path, run_root: &Path) -> Result<Self> {
        let path = run_root.join("workspace-prepared.json");
        if !std::fs::symlink_metadata(&path)?.file_type().is_file() {
            return Err("prepared run selection is not a regular file".into());
        }
        let pointer: PreparedWorkspacePointer = serde_json::from_slice(&std::fs::read(path)?)?;
        Self::load_prepared(workspace, pointer, false)
    }

    /// Explicit source capture for preparation, checks and developer consumers.
    pub(crate) fn load(workspace: &Path, run_root: &Path) -> Result<Self> {
        let pointer = run_root.join("workspace-prepared.json");
        match std::fs::symlink_metadata(&pointer) {
            Ok(_) => return Self::load_prepared_run(workspace, run_root),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let deployment = tidepool_toolchain::toolchain::configured_module_package()?
            .as_ref()
            .map(DeploymentSources::from_package);
        Self::load_with_deployment(workspace, run_root, deployment)
    }

    fn load_with_deployment(
        workspace: &Path,
        run_root: &Path,
        deployment: Option<DeploymentSources>,
    ) -> Result<Self> {
        Self::load_with_deployment_selection(workspace, run_root, deployment, None)
    }

    fn load_with_deployment_selection(
        workspace: &Path,
        run_root: &Path,
        deployment: Option<DeploymentSources>,
        pinned_selection: Option<Vec<u8>>,
    ) -> Result<Self> {
        if deployment
            .as_ref()
            .is_some_and(|selection| selection.version != 1)
        {
            return Err("unsupported runtime library deployment source format".into());
        }
        let directory = run_root.join("workspace");
        let manifest = directory.join("selection.json");
        if pinned_selection.is_some() || manifest.exists() {
            let bytes = match pinned_selection {
                Some(bytes) => bytes,
                None => std::fs::read(&manifest)?,
            };
            let selection: serde_json::Value = serde_json::from_slice(&bytes)?;
            if selection.get("tools").is_some_and(|tools| !tools.is_null()) {
                return Err("frozen workspace uses obsolete [haskell] tools; migrate to spec = 'Module.agentSpec' with agentSpec = defaultSpec { specTools = yourTools }, then start a new run".into());
            }
            if selection.get("version").and_then(serde_json::Value::as_u64) != Some(6) {
                return Err("unsupported frozen workspace format; prepare a new deployment and start a new run".into());
            }
            let mut frozen: Self = serde_json::from_value(selection)?;
            if frozen.library_identity != crate::haskell_sources::source_identity()? {
                return Err(
                    "frozen workspace library differs from this build; start a new swarm".into(),
                );
            }
            if frozen.core_identity != generated_core_identity()? {
                return Err(
                    "frozen workspace generated Core differs from this build; start a new swarm"
                        .into(),
                );
            }
            if frozen.runtime_libraries.deployment() != deployment.as_ref() {
                return Err("frozen runtime library deployment changed; start a new swarm".into());
            }
            let capture_root = directory.canonicalize()?;
            if frozen.include.is_empty() {
                return Err("frozen workspace lacks its generated resource root".into());
            }
            for root in frozen
                .include
                .iter()
                .chain(std::iter::once(&frozen.runtime_orchestration))
            {
                if root.canonicalize()? != *root || !root.starts_with(&capture_root) {
                    return Err(
                        "frozen workspace source root is outside its original capture".into(),
                    );
                }
            }
            let runtime_roots = frozen.runtime_libraries.roots();
            if let RuntimeLibraries::Captured { .. } = &frozen.runtime_libraries {
                for (root, sentinel) in runtime_roots
                    .iter()
                    .zip(["Tidepool/Prelude.hs", "Tidepool/Check.hs"])
                {
                    if !root.canonicalize()?.starts_with(&capture_root)
                        || !root.join(sentinel).is_file()
                    {
                        return Err("frozen runtime library is outside its run capture".into());
                    }
                }
            }
            if let Some(selection) = frozen.runtime_libraries.deployment() {
                verify_deployment_sources(selection)?;
            }
            crate::haskell_sources::verify_runtime_capture(
                &runtime_roots,
                &frozen.runtime_capture_identity,
            )?;
            let captured_identity = tidepool_toolchain::cache::source_roots_identity(
                crate::haskell_sources::DEV_SOURCE_DOMAIN,
                &runtime_roots,
            )?;
            if captured_identity != frozen.runtime_capture_identity {
                return Err("frozen runtime library source changed".into());
            }
            for (relative, hash) in &frozen.files {
                if relative.is_absolute()
                    || relative
                        .components()
                        .any(|part| !matches!(part, std::path::Component::Normal(_)))
                {
                    return Err("frozen workspace file inventory escapes its capture".into());
                }
                if !matches!(
                    frozen.preparation,
                    Some(WorkspacePreparation::Completed { .. })
                ) {
                    let bytes = std::fs::read(directory.join(relative))?;
                    if blake3::hash(&bytes).to_hex().as_str() != hash {
                        return Err(format!(
                            "frozen workspace input changed: {}",
                            relative.display()
                        )
                        .into());
                    }
                }
            }
            let generated = tidepool_mcp::ensure_effects_module(
                &crate::actor_host::exomonad_effect_declarations(),
            )?;
            if inspect_sources(&frozen.runtime_orchestration)?
                != inspect_sources(&generated.orchestration)?
            {
                return Err("frozen workspace orchestration differs from this build".into());
            }
            if let Some(WorkspacePreparation::Completed {
                revision, ..
            }) = &frozen.preparation
            {
                if revision.len() != 64
                    || !revision.bytes().all(|byte| byte.is_ascii_hexdigit())
                {
                    return Err(
                        "completed workspace preparation lacks its original revision or entries"
                            .into(),
                    );
                }
                frozen.validate_prepared_toolset_promises()?;
                if deployment_files(&directory)? != frozen.files {
                    return Err("prepared workspace deployment inventory changed".into());
                }
                frozen.prepared_deployment = Some(std::sync::Arc::new(
                    tidepool_atomic_write::DirectoryAnchor::open_existing(run_root)?,
                ));
            }
            return Ok(frozen);
        }
        let (config, config_text) = super::read_project_config(workspace)?;
        for (alias, model) in &config.models {
            if alias.trim().is_empty() || model.trim().is_empty() {
                return Err("model aliases and provider model names must be non-empty".into());
            }
        }
        let base = workspace.join(".exomonad");
        let runtime_sources = crate::haskell_sources::runtime_source_roots(
            deployment.as_ref().map(|selection| &selection.sources),
        )?;
        let runtime_identity = crate::haskell_sources::runtime_capture_identity(&runtime_sources)?;
        std::fs::create_dir_all(&directory)?;
        let mut files = BTreeMap::new();
        let mut include = Vec::new();
        // Failed captures have no manifest. A retry selects fresh directories,
        // so deleted modules from a partial attempt cannot remain importable.
        let capture = uuid::Uuid::new_v4();
        let roots = resolve_source_roots(workspace, &config.haskell)?;
        for (index, source) in roots.iter().enumerate() {
            let relative = PathBuf::from(format!("sources/{capture}/{index}"));
            capture_sources(source, &relative, &directory, &mut files)?;
            include.push(directory.join(relative));
        }
        let runtime_libraries = capture_selected_runtime_libraries(
            &runtime_sources,
            &runtime_identity,
            deployment,
            &directory,
            capture,
            &mut files,
        )?;
        let generated = tidepool_mcp::ensure_effects_module(
            &crate::actor_host::exomonad_effect_declarations(),
        )?;
        capture_sources(
            &generated.orchestration,
            Path::new("orchestration"),
            &directory,
            &mut files,
        )?;
        let runtime_orchestration = directory.join("orchestration");
        for entry in config
            .haskell
            .checks
            .iter()
            .chain(config.haskell.spec.iter())
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
        let library_identity = crate::haskell_sources::source_identity()?;
        let core_identity = generated_core_identity()?;
        let source_prefix = PathBuf::from(format!("sources/{capture}"));
        // Library bytes are represented by the build-bound identity above.
        // Their per-run capture UUID must not change an otherwise identical
        // workspace selection's logical identity.
        let library_prefix = PathBuf::from(format!("libraries/{capture}"));
        let logical_files = files
            .iter()
            .filter(|(path, _)| !path.starts_with(&library_prefix))
            .map(|(path, hash)| (path.strip_prefix(&source_prefix).unwrap_or(path), hash))
            .collect::<Vec<_>>();
        let identity = blake3::hash(&serde_json::to_vec(&(
            &config_text,
            &library_identity,
            &core_identity,
            runtime_libraries.deployment(),
            &prompts,
            logical_files,
        ))?)
        .to_hex()
        .to_string();
        let workspace_root = authored_workspace_root(&config.haskell.source_roots);
        let resources = resource_module(
            &identity,
            &workspace_root,
            &config.haskell.modules,
            &prompts,
        );
        let resources_path = PathBuf::from("resources/Exomonad/Workspace.hs");
        if include.iter().any(|root| generates_workspace_module(root)) {
            return Err(
                "Exomonad.Workspace is reserved for the generated workspace interface".into(),
            );
        }
        std::fs::create_dir_all(directory.join("resources/Exomonad"))?;
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
            version: 6,
            identity,
            include,
            modules: config.haskell.modules,
            checks: config.haskell.checks,
            spec: config.haskell.spec,
            prompts,
            models: config.models,
            files,
            config: config_text,
            library_identity,
            core_identity,
            runtime_libraries,
            runtime_capture_identity: runtime_identity,
            runtime_orchestration,
            preparation: None,
            prepared_deployment: None,
        };
        tidepool_atomic_write::write_durable(&manifest, &serde_json::to_vec_pretty(&frozen)?)?;
        Ok(frozen)
    }

    fn load_prepared(
        workspace: &Path,
        pointer: PreparedWorkspacePointer,
        check_current: bool,
    ) -> Result<Self> {
        let selection = pointer.read_selection()?;
        let deployment = tidepool_toolchain::toolchain::configured_module_package()?
            .as_ref()
            .map(DeploymentSources::from_package);
        let frozen = Self::load_with_deployment_selection(
            workspace,
            &pointer.directory,
            deployment,
            Some(selection),
        )?;
        if !matches!(
            frozen.preparation,
            Some(WorkspacePreparation::Completed { .. })
        ) {
            return Err(
                "workspace preparation is incomplete; finish exomonad prepare before init".into(),
            );
        }
        if check_current {
            frozen.verify_current_inputs(workspace)?;
        }
        Ok(frozen)
    }

    pub(crate) fn select_prepared(
        workspace: &Path,
        run_root: &Path,
        explicit: Option<&Path>,
    ) -> Result<Self> {
        let pointer = match explicit {
            Some(directory) => PreparedWorkspacePointer::for_directory(directory)?,
            None => {
                let path = workspace.join(".exomonad/prepared.json");
                if !path.is_file() {
                    return Err("workspace has no completed preparation; run exomonad prepare --directory DIRECTORY".into());
                }
                serde_json::from_slice(&std::fs::read(path)?)?
            }
        };
        let frozen = Self::load_prepared(workspace, pointer.clone(), true)?;
        let path = run_root.join("workspace-prepared.json");
        let bytes = serde_json::to_vec_pretty(&pointer)?;
        if !tidepool_atomic_write::write_durable_new(&path, &bytes)?
            && std::fs::read(&path)? != bytes
        {
            return Err("run already selected another prepared workspace deployment".into());
        }
        Ok(frozen)
    }

    pub(crate) fn begin_preparation(
        workspace: &Path,
        directory: &tidepool_atomic_write::DirectoryAnchor,
    ) -> Result<Self> {
        let mut frozen = Self::load(workspace, directory.path())?;
        frozen.verify_current_inputs(workspace)?;
        frozen.admit_preparation(directory.path())?;
        Ok(frozen)
    }

    fn admit_preparation(&mut self, directory: &Path) -> Result<()> {
        if self.preparation.is_none() {
            self.preparation = Some(WorkspacePreparation::Preparing {
                original: uuid::Uuid::new_v4(),
            });
            self.write_selection(directory)?;
        }
        Ok(())
    }

    pub(crate) fn complete_preparation(
        &mut self,
        directory: &tidepool_atomic_write::DirectoryAnchor,
        revision: String,
        coverage: Vec<PreparedToolsetCoverage>,
    ) -> Result<()> {
        let Some(WorkspacePreparation::Preparing { original }) = &self.preparation else {
            return Err("only an admitted workspace preparation can complete".into());
        };
        if revision.len() != 64
            || !revision.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(
                "workspace preparation must settle original native entries and revision".into(),
            );
        }
        self.validate_toolset_coverage(&coverage)?;
        self.files = deployment_files(&directory.path().join("workspace"))?;
        self.preparation = Some(WorkspacePreparation::Completed {
            original: *original,
            revision,
            coverage,
        });
        self.write_selection(directory.path())?;
        self.prepared_deployment = Some(std::sync::Arc::new(
            tidepool_atomic_write::DirectoryAnchor::open_existing(directory.path())?,
        ));
        self.seal_preparation(directory)
    }

    pub(crate) fn seal_preparation(
        &self,
        directory: &tidepool_atomic_write::DirectoryAnchor,
    ) -> Result<()> {
        if !matches!(
            self.preparation,
            Some(WorkspacePreparation::Completed { .. })
        ) {
            return Err("only a completed workspace preparation can be sealed".into());
        }
        self.validate_prepared_toolset_promises()?;
        seal_prepared_directory(directory.path())
    }

    pub(crate) fn prepared_toolset_coverage(&self) -> Option<&[PreparedToolsetCoverage]> {
        match &self.preparation {
            Some(WorkspacePreparation::Completed { coverage, .. }) => Some(coverage),
            _ => None,
        }
    }

    pub(crate) fn completed_entry_selections(&self) -> Result<BTreeMap<String, uuid::Uuid>> {
        coverage_entries(
            self.prepared_toolset_coverage()
                .ok_or("workspace toolset preparation is incomplete")?,
        )
    }

    fn validate_toolset_coverage(&self, coverage: &[PreparedToolsetCoverage]) -> Result<()> {
        coverage_entries(coverage)?;
        let config = self.config()?;
        let expected = config.preparation.selected_profiles(config.research)?;
        if expected.len() != coverage.len() {
            return Err("prepared workspace does not cover all configured toolset profiles".into());
        }
        for profile in expected {
            let entry = coverage
                .iter()
                .find(|entry| entry.profile == profile.profile)
                .ok_or("prepared workspace is missing a configured toolset profile")?;
            if entry.requested_effects != profile.requested_effects {
                return Err("prepared toolset profile differs from this build's requested effect row".into());
            }
        }
        Ok(())
    }

    fn validate_prepared_toolset_promises(&self) -> Result<()> {
        self.validate_toolset_coverage(
            self.prepared_toolset_coverage()
                .ok_or("workspace toolset preparation is incomplete")?,
        )
    }

    /// Check every promised specialization against actual assembled support
    /// before actor startup can interpret an absent recipe as uncovered work.
    pub(crate) fn validate_prepared_toolset_recipes(
        &self,
        workbench: &exomonad_actor::ActorWorkbenchSource,
        source: &exomonad_actor::CheckpointSourceLayer,
        supported_effects: &[exomonad_tool::ToolEffectKey],
    ) -> Result<()> {
        self.validate_prepared_toolset_promises()?;
        for entry in self.prepared_toolset_coverage().expect("promises validated") {
            let actual = workbench.source_toolset_recipe(
                source,
                &entry.requested_effects,
                supported_effects,
            )?;
            if actual.effective_effects != entry.effective_effects || actual.recipe != entry.recipe {
                return Err("prepared toolset recipe differs from actual host source or interpreter support; prepare a new deployment".into());
            }
        }
        Ok(())
    }

    fn write_selection(&self, directory: &Path) -> Result<()> {
        tidepool_atomic_write::write_durable(
            &directory.join("workspace/selection.json"),
            &serde_json::to_vec_pretty(self)?,
        )?;
        Ok(())
    }

    pub(crate) fn runtime_orchestration(&self) -> PathBuf {
        self.runtime_orchestration.clone()
    }

    pub(crate) fn workspace_resources(&self) -> PathBuf {
        self.include
            .last()
            .expect("validated workspace resource root")
            .clone()
    }

    pub(crate) fn prepared_source_revision(&self) -> Result<Option<PathBuf>> {
        match &self.preparation {
            Some(WorkspacePreparation::Completed { revision, .. }) => {
                let deployment = self
                    .prepared_deployment
                    .as_ref()
                    .ok_or("completed preparation has no acquired deployment owner")?;
                Ok(Some(
                    deployment.path().join("workspace/revisions").join(revision),
                ))
            }
            _ => Ok(None),
        }
    }

    pub(crate) fn verify_current_inputs(&self, workspace: &Path) -> Result<()> {
        let (config, text) = super::read_project_config(workspace)?;
        if text != self.config {
            return Err(
                "workspace configuration changed since preparation; prepare a new deployment"
                    .into(),
            );
        }
        let roots = resolve_source_roots(workspace, &config.haskell)?;
        if roots.len() != self.captured_source_roots().len() {
            return Err("workspace source roots changed since preparation".into());
        }
        for (current, captured) in roots.iter().zip(self.captured_source_roots()) {
            if inspect_sources(current)? != inspect_sources(captured)? {
                return Err(
                    "workspace source bytes changed since preparation; prepare a new deployment"
                        .into(),
                );
            }
        }
        let base = workspace.join(".exomonad");
        for (name, path) in [
            ("core", config.prompts.core),
            ("root", config.prompts.root),
            ("research", config.prompts.research),
            ("coding", config.prompts.coding),
            ("scaffolding", config.prompts.scaffolding),
            ("integration", config.prompts.integration),
        ]
        .into_iter()
        .filter_map(|(name, path)| path.map(|path| (name.to_owned(), path)))
        .chain(config.prompts.files)
        {
            if self.prompts.get(&name) != Some(&std::fs::read_to_string(base.join(path))?) {
                return Err(
                    "workspace prompt changed since preparation; prepare a new deployment".into(),
                );
            }
        }
        Ok(())
    }

    pub(crate) fn identity(&self) -> &str {
        &self.identity
    }

    pub(crate) fn runtime_stdlib(&self) -> PathBuf {
        self.runtime_libraries.stdlib()
    }

    pub(crate) fn runtime_actors(&self) -> PathBuf {
        self.runtime_libraries.actors()
    }

    pub(crate) fn runtime_catalog_roots(&self) -> Option<Vec<PathBuf>> {
        self.runtime_libraries
            .deployment()
            .map(|selection| selection.sources.include_roots())
    }

    /// This run's verified capture of the workspace's source roots, in search
    /// order. The generated resources directory `freeze` appends is not one of
    /// them: it holds the workspace interface, not authored source.
    pub(crate) fn captured_source_roots(&self) -> &[PathBuf] {
        &self.include[..self.include.len().saturating_sub(1)]
    }

    /// Whether this run's captured source carries a module, wherever it came
    /// from — an authored root or a pinned flake input.
    ///
    /// The Jev authoring surface is the case this exists for. It is pinned
    /// source a project opts into through `[haskell.flake_sources]`, not part
    /// of the Tidepool library, so the workbench offers it under `J` only
    /// where the project supplies it and a run without it compiles unchanged.
    pub(crate) fn provides_module(&self, module: &str) -> bool {
        let relative = module.replace('.', "/");
        self.captured_source_roots().iter().any(|root| {
            root.join(format!("{relative}.hs")).is_file()
                || root.join(format!("{relative}.lhs")).is_file()
        })
    }

    pub(crate) fn imports(&self) -> Vec<String> {
        self.import_modules()
            .map(|module| format!("import {module}"))
            .collect()
    }

    pub(crate) fn import_modules(&self) -> impl Iterator<Item = &str> {
        std::iter::once("Exomonad.Workspace").chain(self.modules.iter().map(String::as_str))
    }

    pub(crate) fn config(&self) -> Result<super::ExomonadConfig> {
        let config: super::ExomonadConfig = toml::from_str(&self.config)?;
        config.launch.validate()?;
        Ok(config)
    }
}

fn deployment_files(directory: &Path) -> Result<BTreeMap<PathBuf, String>> {
    fn walk(root: &Path, relative: &Path, files: &mut BTreeMap<PathBuf, String>) -> Result<()> {
        for entry in std::fs::read_dir(root.join(relative))? {
            let entry = entry?;
            let path = relative.join(entry.file_name());
            if path == Path::new("selection.json") {
                continue;
            }
            let metadata = entry.file_type()?;
            if metadata.is_dir() {
                walk(root, &path, files)?;
            } else if metadata.is_file() {
                files.insert(
                    path,
                    blake3::hash(&std::fs::read(entry.path())?)
                        .to_hex()
                        .to_string(),
                );
            } else if metadata.is_symlink() {
                // Active source publication is a pointer, never copied compiler bytes.
                let target = std::fs::read_link(entry.path())?;
                if entry.path().canonicalize()?.starts_with(root) {
                    files.insert(
                        path,
                        blake3::hash(target.as_os_str().as_encoded_bytes())
                            .to_hex()
                            .to_string(),
                    );
                } else {
                    return Err("prepared workspace alias escapes its owned deployment".into());
                }
            } else {
                return Err("unsupported prepared workspace file".into());
            }
        }
        Ok(())
    }
    let mut files = BTreeMap::new();
    walk(directory, Path::new(""), &mut files)?;
    Ok(files)
}

fn seal_prepared_directory(directory: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let metadata = entry.file_type()?;
        if metadata.is_dir() {
            seal_prepared_directory(&entry.path())?;
        } else if metadata.is_file() {
            let mut permissions = entry.metadata()?.permissions();
            permissions.set_mode(permissions.mode() & !0o222);
            std::fs::set_permissions(entry.path(), permissions)?;
        }
    }
    let mut permissions = directory.metadata()?.permissions();
    permissions.set_mode(permissions.mode() & !0o222);
    std::fs::set_permissions(directory, permissions)?;
    Ok(())
}

/// The generated interface is appended as the last source root. An authored
/// file at its exact module path would either shadow that interface or make
/// the same module name resolve differently across source roots. Other
/// `Exomonad.*` modules are ordinary workspace source.
fn generates_workspace_module(root: &Path) -> bool {
    ["hs", "lhs", "hs-boot", "lhs-boot"]
        .into_iter()
        .any(|suffix| root.join(format!("Exomonad/Workspace.{suffix}")).is_file())
}

fn generated_core_identity() -> Result<String> {
    let root = tidepool_mcp::ensure_effects_core_module()?;
    Ok(root
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("generated Core directory has no valid name")?
        .to_owned())
}

/// The project-relative path of the authored workspace directory: the last
/// configured `[haskell] source_roots` entry, resolved against `.exomonad`.
///
/// A project names its own package directly as `source_roots = ["."]`, or
/// layers a checked-out workspace after a shared root as
/// `source_roots = [".", "workspace"]` (the submodule layout `exomonad new`
/// writes via `add_default_workspace`). Either way the last entry is the
/// checkout that actually carries `Project/*.hs` and the `checks/` fixtures
/// recipes read; earlier entries, when present, contribute only shared files
/// like `AgentSpec.hs`. One resolver, reused by every recipe site that builds
/// a fixture path instead of each hardcoding where the workspace lives.
fn authored_workspace_root(source_roots: &[PathBuf]) -> String {
    let last = source_roots
        .last()
        .map(PathBuf::as_path)
        .unwrap_or_else(|| Path::new("."));
    let joined: PathBuf = Path::new(".exomonad")
        .join(last)
        .components()
        .filter(|component| !matches!(component, std::path::Component::CurDir))
        .collect();
    joined.to_string_lossy().replace('\\', "/")
}

fn resource_module(
    identity: &str,
    workspace_root: &str,
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
        "{}\nworkspaceIdentity = {}\nworkspaceRoot = {}\nworkspaceModules = [{}]\nworkspacePrompts = [{}]\n",
        include_str!("workspace.hs"),
        literal(identity),
        literal(workspace_root),
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

/// The workspace's Haskell source roots, in search order: the authored
/// `[haskell] source_roots` relative to `.exomonad`, then whatever
/// `[haskell.flake_sources]` selects out of the project's flake inputs.
///
/// One resolver, used both when a run freezes its capture and when a live
/// reload re-reads the same roots, so a reload cannot silently read a
/// different set of directories than the run started from.
pub(super) fn resolve_source_roots(
    workspace: &Path,
    config: &HaskellConfig,
) -> Result<Vec<PathBuf>> {
    let base = workspace.join(".exomonad");
    let mut roots = Vec::new();
    for root in &config.source_roots {
        roots.push(base.join(root).canonicalize()?);
    }
    roots.extend(flake_source_roots(workspace, config)?);
    Ok(roots)
}

/// The `nix` executable Exomonad invokes. One resolution, shared by the fetch
/// that materializes a run's pinned inputs and by the lock `exomonad new` writes.
pub(super) fn nix_bin() -> PathBuf {
    std::env::var_os(super::ENV_NIX_BIN).map_or_else(|| PathBuf::from("nix"), PathBuf::from)
}

/// Locking and source capture share the declared executable and network policy.
pub(super) fn nix_command() -> std::process::Command {
    #[allow(
        clippy::disallowed_methods,
        reason = "one-shot Nix source admission; not a long-lived child"
    )]
    let mut command = std::process::Command::new(nix_bin());
    if std::env::var_os(super::ENV_NIX_OFFLINE).is_some() {
        command.arg("--offline");
    }
    command
}

/// Fetch the project's flake inputs and return the Haskell source directories
/// `[haskell.flake_sources]` selects from them, ordered by input name.
///
/// `nix flake archive` locks the project's `flake.nix`, fetches every input,
/// and reports where each one landed. The returned directories are ordinary
/// source roots from that point on: the caller captures them into this run's
/// frozen workspace exactly like an authored `source_roots` entry, so a pinned
/// dependency compiles through the same pipeline, reaches every actor through
/// the same resident include list, and is fingerprinted by the same content
/// walk. There is no separate dependency registry and no precompiled package.
///
/// The workspace is handed to `nix` the way any other `nix` command receives a
/// project directory, so a Git workspace contributes its tracked working-tree
/// files and a dirty `flake.nix` is read as written.
///
/// `[haskell.flake_overrides]` replaces an input with a local directory for
/// this run. The project's `flake.lock` is left alone while an override is in
/// play, so editing a dependency in place stays a working change rather than a
/// re-pin.
/// What `nix flake archive` answered for one project, and the state of the
/// flake files it answered for.
type ArchivedRoots = ((PathBuf, String), (FileStamp, FileStamp), Vec<PathBuf>);

/// A file's size and modification time, or `None` when it is absent.
type FileStamp = Option<(u64, std::time::SystemTime)>;

fn file_stamp(path: &Path) -> FileStamp {
    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.len(), metadata.modified().ok()?))
}

/// [`archive_flake_sources`], remembered while the project's flake files are
/// unchanged.
///
/// A pinned input is a store path fixed by `flake.lock`, so asking again while
/// `flake.nix` and `flake.lock` are unchanged evaluates the flake to learn what
/// is already known. Source status and drift are read on a timer for every
/// actor, which made that one `nix` process per actor every few seconds. An
/// override names a local directory whose contents can change under the same
/// flake files, so an overridden project is always asked again.
fn flake_source_roots(workspace: &Path, config: &HaskellConfig) -> Result<Vec<PathBuf>> {
    static REMEMBERED: std::sync::Mutex<Vec<ArchivedRoots>> = std::sync::Mutex::new(Vec::new());
    if config.flake_sources.is_empty() || !config.flake_overrides.is_empty() {
        return archive_flake_sources(workspace, config);
    }
    let key = (
        workspace.to_path_buf(),
        format!("{:?}", config.flake_sources),
    );
    let stamp = (
        file_stamp(&workspace.join("flake.nix")),
        file_stamp(&workspace.join("flake.lock")),
    );
    if let Ok(remembered) = REMEMBERED.lock() {
        if let Some((_, _, roots)) = remembered
            .iter()
            .find(|(known, known_stamp, _)| *known == key && *known_stamp == stamp)
        {
            return Ok(roots.clone());
        }
    }
    let roots = archive_flake_sources(workspace, config)?;
    // The lock file may have been written by the call above; stamp what is
    // there now, so the next read matches it.
    let stamp = (
        file_stamp(&workspace.join("flake.nix")),
        file_stamp(&workspace.join("flake.lock")),
    );
    if let Ok(mut remembered) = REMEMBERED.lock() {
        remembered.retain(|(known, _, _)| *known != key);
        remembered.push((key, stamp, roots.clone()));
    }
    Ok(roots)
}

fn archive_flake_sources(workspace: &Path, config: &HaskellConfig) -> Result<Vec<PathBuf>> {
    for input in config.flake_overrides.keys() {
        if !config.flake_sources.contains_key(input) {
            return Err(format!(
                "[haskell.flake_overrides] names {input:?}, which [haskell.flake_sources] does not use"
            )
            .into());
        }
    }
    if config.flake_sources.is_empty() {
        return Ok(Vec::new());
    }
    if !workspace.join("flake.nix").is_file() {
        return Err(format!(
            "[haskell.flake_sources] pins source through the project's flake, but {} has no flake.nix",
            workspace.display()
        )
        .into());
    }
    let base = workspace.join(".exomonad");
    let nix = nix_bin();
    let mut command = nix_command();
    command
        .arg("--extra-experimental-features")
        .arg("nix-command flakes")
        .args(["flake", "archive", "--json"]);
    if !config.flake_overrides.is_empty() {
        command.arg("--no-write-lock-file");
    }
    // `--override-input <name> path:<dir>` copies the whole directory into
    // the nix store (a `path:` URL, chosen deliberately elsewhere so the
    // workspace itself is handed to nix as a bare directory rather than
    // copying build trees). Fine for a small sibling checkout; slow for one
    // with build artifacts alongside it.
    for (input, path) in &config.flake_overrides {
        let directory = base.join(path).canonicalize()?;
        let directory = directory.to_str().ok_or_else(|| {
            format!("[haskell.flake_overrides] {input:?} is not a text path: {directory:?}")
        })?;
        command
            .arg("--override-input")
            .arg(input)
            .arg(format!("path:{directory}"));
    }
    let report = command.arg(workspace).output().map_err(|error| {
        format!(
            "cannot start {} to fetch flake inputs: {error}",
            nix.display()
        )
    })?;
    if !report.status.success() {
        return Err(format!(
            "cannot fetch the project's flake inputs ({}): {}",
            report.status,
            String::from_utf8_lossy(&report.stderr).trim()
        )
        .into());
    }
    #[derive(Deserialize)]
    struct Archive {
        #[serde(default)]
        inputs: BTreeMap<String, ArchivedInput>,
    }
    #[derive(Deserialize)]
    struct ArchivedInput {
        #[serde(default)]
        path: Option<PathBuf>,
    }
    let archive: Archive = serde_json::from_slice(&report.stdout)?;
    let mut roots = Vec::new();
    for (input, directories) in &config.flake_sources {
        let fetched = archive
            .inputs
            .get(input)
            .ok_or_else(|| format!("the project's flake.nix declares no input named {input:?}"))?;
        let fetched_path = fetched
            .path
            .as_ref()
            .ok_or_else(|| format!("flake input {input:?} has no archived source path"))?;
        if directories.is_empty() {
            return Err(
                format!("[haskell.flake_sources] {input:?} names no source directory").into(),
            );
        }
        for directory in directories {
            if directory.is_absolute()
                || directory.components().any(|part| {
                    !matches!(
                        part,
                        std::path::Component::Normal(_) | std::path::Component::CurDir
                    )
                })
            {
                return Err(format!(
                    "[haskell.flake_sources] {input:?} directory must stay inside the input: {}",
                    directory.display()
                )
                .into());
            }
            let root = fetched_path.join(directory);
            if !root.is_dir() {
                return Err(format!(
                    "flake input {input:?} has no directory {}",
                    directory.display()
                )
                .into());
            }
            roots.push(root);
        }
    }
    Ok(roots)
}

/// Copy the authored package into an isolated check repository, excluding
/// runtime trees.
///
/// The project's `flake.nix` and `flake.lock` travel with it: a package whose
/// `[haskell.flake_sources]` names pinned Haskell does not compile without the
/// pin, so a copy that left them behind would be a package the copy cannot
/// build. The copy is files only; `nix` reads a Git tree's tracked files, so a
/// destination that is a repository has to commit what it received.
pub(crate) fn copy_authored(workspace: &Path, destination: &Path) -> Result<()> {
    capture_tree(
        &workspace.join(".exomonad"),
        Path::new(".exomonad"),
        destination,
        &mut BTreeMap::new(),
        true,
    )?;
    for name in ["flake.nix", "flake.lock"] {
        if workspace.join(name).is_file() {
            std::fs::copy(workspace.join(name), destination.join(name))?;
        }
    }
    Ok(())
}

/// A cheap signature of what [`capture_sources`] would read from `roots`: every
/// captured file's path, size and modification time, and nothing of its
/// contents. Two equal signatures mean a capture would produce the same
/// revision, so a reader on a timer can skip the copy. A root inside the Nix
/// store is immutable and is signed by its path alone.
pub(super) fn sources_signature(roots: &[PathBuf]) -> Result<String> {
    fn walk(directory: &Path, hasher: &mut blake3::Hasher) -> Result<()> {
        let mut entries: Vec<_> = std::fs::read_dir(directory)?.collect::<std::io::Result<_>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let name = entry.file_name();
            if matches!(
                name.to_str(),
                Some(
                    "logs" | "sessions" | "runtime" | "build" | ".git" | "dist-newstyle" | "target"
                )
            ) {
                continue;
            }
            let kind = entry.file_type()?;
            if kind.is_dir() {
                walk(&entry.path(), hasher)?;
            } else if kind.is_symlink()
                || matches!(
                    entry.path().extension().and_then(|x| x.to_str()),
                    Some("hs" | "lhs" | "hs-boot" | "h")
                )
            {
                // A symlink makes a capture fail; signing it keeps that
                // failure from being hidden behind an unchanged signature.
                let metadata = entry.metadata()?;
                hasher.update(entry.path().as_os_str().as_encoded_bytes());
                hasher.update(&metadata.len().to_le_bytes());
                if let Ok(elapsed) = metadata.modified().and_then(|at| {
                    at.duration_since(std::time::UNIX_EPOCH)
                        .map_err(std::io::Error::other)
                }) {
                    hasher.update(&elapsed.as_nanos().to_le_bytes());
                }
            }
        }
        Ok(())
    }
    let mut hasher = blake3::Hasher::new();
    for root in roots {
        hasher.update(root.as_os_str().as_encoded_bytes());
        if !root.starts_with("/nix/store") {
            walk(root, &mut hasher)?;
        }
    }
    Ok(hasher.finalize().to_hex().to_string())
}

pub(super) fn capture_sources(
    source: &Path,
    relative: &Path,
    destination: &Path,
    files: &mut BTreeMap<PathBuf, String>,
) -> Result<()> {
    capture_tree(source, relative, destination, files, false)
}

fn capture_selected_runtime_libraries(
    sources: &[PathBuf; 2],
    expected_identity: &str,
    deployment: Option<DeploymentSources>,
    directory: &Path,
    capture: uuid::Uuid,
    files: &mut BTreeMap<PathBuf, String>,
) -> Result<RuntimeLibraries> {
    let Some(selection) = deployment else {
        let [stdlib, actors] =
            capture_runtime_libraries(sources, expected_identity, directory, capture, files)?;
        return Ok(RuntimeLibraries::Captured { stdlib, actors });
    };
    verify_deployment_sources(&selection)?;
    let selected = [
        selection.sources.root(NativeSourceRole::Stdlib),
        selection.sources.root(NativeSourceRole::Actors),
    ];
    if selected != [sources[0].canonicalize()?, sources[1].canonicalize()?] {
        return Err("relocated runtime library deployment".into());
    }
    let selected_identity = tidepool_toolchain::cache::source_roots_identity(
        crate::haskell_sources::DEV_SOURCE_DOMAIN,
        &selected,
    )?;
    if selected_identity != expected_identity {
        return Err("runtime Haskell library changed during run capture".into());
    }
    crate::haskell_sources::verify_runtime_capture(&selected, expected_identity)?;
    Ok(RuntimeLibraries::Deployment { selection })
}

fn verify_deployment_sources(selection: &DeploymentSources) -> Result<()> {
    if selection.version != 1
        || selection.sources.roles != NativeSourceRole::ORDERED
        || selection.sources.snapshot_root.canonicalize()? != selection.sources.snapshot_root
    {
        return Err("unsupported or relocated runtime library deployment".into());
    }
    for root in selection.sources.include_roots() {
        if !root.is_dir() || root.canonicalize()? != root {
            return Err("missing or aliased runtime library deployment root".into());
        }
    }
    // Inspect before hashing so source aliases cannot enter a retained selection.
    inspect_sources(&selection.sources.snapshot_root)?;
    if NativeCatalogSourceSelection::source_manifest(&selection.sources.snapshot_root)?
        != selection.sources.source_files
    {
        return Err("runtime library deployment source changed".into());
    }
    Ok(())
}

/// Inspect the same files and enforce the same source-tree policy as capture,
/// without publishing or writing any source bytes.
pub(super) fn inspect_sources(source: &Path) -> Result<BTreeMap<PathBuf, String>> {
    let mut files = BTreeMap::new();
    inspect_source_tree(source, Path::new(""), None, &mut files, false)?;
    Ok(files)
}

pub(super) fn is_haskell_source(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("hs" | "lhs" | "hs-boot" | "lhs-boot" | "h")
    )
}

fn capture_runtime_libraries(
    sources: &[PathBuf; 2],
    expected_identity: &str,
    directory: &Path,
    capture: uuid::Uuid,
    files: &mut BTreeMap<PathBuf, String>,
) -> Result<[PathBuf; 2]> {
    let relative = [
        PathBuf::from(format!("libraries/{capture}/stdlib")),
        PathBuf::from(format!("libraries/{capture}/actors")),
    ];
    for (source, path) in sources.iter().zip(&relative) {
        capture_sources(source, path, directory, files)?;
    }
    let captured = [directory.join(&relative[0]), directory.join(&relative[1])];
    let captured_identity = tidepool_toolchain::cache::source_roots_identity(
        crate::haskell_sources::DEV_SOURCE_DOMAIN,
        &captured,
    )?;
    let source_identity = tidepool_toolchain::cache::source_roots_identity(
        crate::haskell_sources::DEV_SOURCE_DOMAIN,
        sources,
    )?;
    if captured_identity != expected_identity || source_identity != expected_identity {
        return Err("runtime Haskell library changed during run capture".into());
    }
    crate::haskell_sources::verify_runtime_capture(&captured, expected_identity)?;
    Ok(captured)
}

fn capture_tree(
    source: &Path,
    relative: &Path,
    destination: &Path,
    files: &mut BTreeMap<PathBuf, String>,
    all_authored: bool,
) -> Result<()> {
    inspect_source_tree(source, relative, Some(destination), files, all_authored)
}

fn inspect_source_tree(
    source: &Path,
    relative: &Path,
    destination: Option<&Path>,
    files: &mut BTreeMap<PathBuf, String>,
    all_authored: bool,
) -> Result<()> {
    if let Some(destination) = destination {
        std::fs::create_dir_all(destination.join(relative))?;
    }
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        // Source roots may be `.exomonad` itself. Runtime/build trees are not inputs.
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
            inspect_source_tree(&entry.path(), &path, destination, files, all_authored)?;
        } else if all_authored || is_haskell_source(&entry.path()) {
            let bytes = std::fs::read(entry.path())?;
            if let Some(destination) = destination {
                tidepool_atomic_write::write_durable(&destination.join(&path), &bytes)?;
            }
            files.insert(path, blake3::hash(&bytes).to_hex().to_string());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::disallowed_methods,
        reason = "test: launches short-lived git one-shots to build fixture repositories"
    )]
    use super::*;

    // Workspace tests supply a private selection mirror. Production constructs
    // that mirror only from the toolchain's admitted immutable module package.
    fn deployment_fixture() -> (tempfile::TempDir, DeploymentSources) {
        let fixture = tempfile::tempdir().unwrap();
        let sources = crate::haskell_sources::runtime_source_roots(None).unwrap();
        capture_sources(
            &sources[0],
            Path::new("lib"),
            fixture.path(),
            &mut BTreeMap::new(),
        )
        .unwrap();
        capture_sources(
            &sources[1],
            Path::new("actors"),
            fixture.path(),
            &mut BTreeMap::new(),
        )
        .unwrap();
        for role in NativeSourceRole::ORDERED {
            std::fs::create_dir_all(fixture.path().join(role.relative_root())).unwrap();
        }
        let selection = DeploymentSources {
            version: 1,
            sources: NativeCatalogSourceSelection {
                snapshot_root: fixture.path().canonicalize().unwrap(),
                roles: NativeSourceRole::ORDERED,
                source_files: NativeCatalogSourceSelection::source_manifest(fixture.path())
                    .unwrap(),
            },
            source_pin: "fixture-source".into(),
            producer_identity: [7; 32],
            catalog_identity: "fixture-catalog".into(),
        };
        (fixture, selection)
    }

    fn deployment_project(config: &str) -> tempfile::TempDir {
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(project.path().join(".exomonad")).unwrap();
        std::fs::write(project.path().join(".exomonad/config.toml"), config).unwrap();
        project
    }

    #[test]
    fn preparing_retry_preserves_original_nonce_and_refuses_mutable_source_drift() {
        let (_libraries, selection) = deployment_fixture();
        let project = deployment_project(
            "[defaults]\nmodel = 'gpt-6-sol'\n[haskell]\nsource_roots = ['src']\n[prompts]\nroot = 'root.md'\n",
        );
        let authored = project.path().join(".exomonad/src");
        std::fs::create_dir_all(&authored).unwrap();
        std::fs::write(
            authored.join("Original.hs"),
            "module Original where\noriginal = (37 :: Int)\n",
        )
        .unwrap();
        let prompt = project.path().join(".exomonad/root.md");
        std::fs::write(&prompt, "original root prompt").unwrap();
        let directory = tempfile::tempdir().unwrap();
        let mut frozen = FrozenWorkspace::load_with_deployment(
            project.path(),
            directory.path(),
            Some(selection.clone()),
        )
        .unwrap();
        frozen.verify_current_inputs(project.path()).unwrap();
        frozen.admit_preparation(directory.path()).unwrap();
        let Some(WorkspacePreparation::Preparing { original }) = frozen.preparation else {
            panic!("preparing nonce must be published before compilation");
        };
        let before = std::fs::read(directory.path().join("workspace/selection.json")).unwrap();
        let mut retry = FrozenWorkspace::load_with_deployment(
            project.path(),
            directory.path(),
            Some(selection),
        )
        .unwrap();
        retry.admit_preparation(directory.path()).unwrap();
        assert!(
            matches!(retry.preparation, Some(WorkspacePreparation::Preparing { original: selected }) if selected == original)
        );
        assert_eq!(
            std::fs::read(directory.path().join("workspace/selection.json")).unwrap(),
            before
        );
        std::fs::write(&prompt, "changed root prompt").unwrap();
        assert!(retry
            .verify_current_inputs(project.path())
            .unwrap_err()
            .to_string()
            .contains("prompt changed"));
        std::fs::write(&prompt, "original root prompt").unwrap();
        std::fs::write(authored.join("Added.hs"), "module Added where\n").unwrap();
        assert!(retry
            .verify_current_inputs(project.path())
            .unwrap_err()
            .to_string()
            .contains("source bytes changed"));
        assert_eq!(
            std::fs::read(directory.path().join("workspace/selection.json")).unwrap(),
            before
        );
    }

    #[test]
    fn qualified_run_refuses_lost_or_unreadable_selection_without_source_capture() {
        use std::os::unix::fs::PermissionsExt;

        let project = deployment_project("[defaults]\nmodel = 'gpt-6-sol'\n");
        let run = tempfile::tempdir().unwrap();
        let pointer = run.path().join("workspace-prepared.json");
        let before = tidepool_extract_cmd::extract_spawn_count();
        let refuse = || {
            assert!(FrozenWorkspace::load_prepared_run(project.path(), run.path()).is_err());
            assert!(std::fs::read_dir(run.path())
                .unwrap()
                .all(|entry| { entry.unwrap().file_name() == "workspace-prepared.json" }));
            assert_eq!(tidepool_extract_cmd::extract_spawn_count(), before);
        };
        // A consumed selection that disappears cannot become a fresh capture.
        std::fs::write(&pointer, b"prior consumed selection").unwrap();
        std::fs::remove_file(&pointer).unwrap();
        refuse();

        std::os::unix::fs::symlink(run.path().join("missing-selection"), &pointer).unwrap();
        refuse();
        assert!(FrozenWorkspace::load(project.path(), run.path()).is_err());
        assert!(!run.path().join("workspace").exists());
        std::fs::remove_file(&pointer).unwrap();

        std::fs::create_dir(&pointer).unwrap();
        refuse();
        std::fs::remove_dir(&pointer).unwrap();

        std::fs::write(&pointer, b"malformed pointer").unwrap();
        refuse();
        std::fs::set_permissions(&pointer, std::fs::Permissions::from_mode(0)).unwrap();
        assert_eq!(
            std::fs::read(&pointer).unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied,
            "this qualification runs under the supported unprivileged account"
        );
        refuse();
        std::fs::set_permissions(&pointer, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[test]
    fn prepared_pointer_refuses_changed_or_removed_metadata_before_decoding() {
        let project = deployment_project("[defaults]\nmodel = 'gpt-6-sol'\n");
        let directory = tempfile::tempdir().unwrap();
        let anchor =
            tidepool_atomic_write::DirectoryAnchor::open_existing(directory.path()).unwrap();
        FrozenWorkspace::begin_preparation(project.path(), &anchor).unwrap();
        let pointer = PreparedWorkspacePointer::for_directory(directory.path()).unwrap();
        let original = pointer.read_selection().unwrap();
        let manifest = directory.path().join("workspace/selection.json");
        let mut selected: serde_json::Value = serde_json::from_slice(&original).unwrap();
        // None of these edits can turn a published selection into an absent
        // recipe and silently enable fresh compilation during run recovery.
        for replacement in [
            serde_json::Value::Null,
            serde_json::json!({"state": "completed", "entries": {}}),
            serde_json::json!({"state": "completed", "entries": {"changed": uuid::Uuid::new_v4()}}),
        ] {
            selected["preparation"] = replacement;
            std::fs::write(&manifest, serde_json::to_vec(&selected).unwrap()).unwrap();
            assert!(pointer
                .read_selection()
                .unwrap_err()
                .to_string()
                .contains("selection changed"));
        }
        // Hash refusal precedes deserialization, including malformed metadata.
        std::fs::write(&manifest, b"invalid selection").unwrap();
        assert!(pointer
            .read_selection()
            .unwrap_err()
            .to_string()
            .contains("selection changed"));
        std::fs::remove_file(&manifest).unwrap();
        assert!(pointer.read_selection().is_err());
        std::fs::write(&manifest, &original).unwrap();
        let legacy = PreparedWorkspacePointer {
            version: 1,
            ..pointer
        };
        assert!(legacy
            .read_selection()
            .unwrap_err()
            .to_string()
            .contains("unsupported"));
    }

    #[test]
    fn selected_preparation_refuses_incomplete_or_legacy_formats_before_run_pointer() {
        let project = deployment_project("[defaults]\nmodel = 'gpt-6-sol'\n");
        let directory = tempfile::tempdir().unwrap();
        let run = tempfile::tempdir().unwrap();
        let anchor =
            tidepool_atomic_write::DirectoryAnchor::open_existing(directory.path()).unwrap();
        FrozenWorkspace::begin_preparation(project.path(), &anchor).unwrap();
        let error =
            FrozenWorkspace::select_prepared(project.path(), run.path(), Some(directory.path()))
                .unwrap_err();
        assert!(error.to_string().contains("incomplete"), "{error}");
        assert!(!run.path().join("workspace-prepared.json").exists());
        let manifest = directory.path().join("workspace/selection.json");
        let mut selected: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
        selected["version"] = serde_json::json!(4);
        std::fs::write(&manifest, serde_json::to_vec(&selected).unwrap()).unwrap();
        let error = FrozenWorkspace::load(project.path(), directory.path()).unwrap_err();
        assert!(error
            .to_string()
            .contains("unsupported frozen workspace format"));
        assert!(!run.path().join("workspace-prepared.json").exists());
    }

    #[test]
    fn deployed_libraries_retain_original_roots_without_run_copy() {
        let (_fixture, selection) = deployment_fixture();
        let project = deployment_project("[defaults]\nmodel = 'gpt-6-sol'\n");
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let frozen = FrozenWorkspace::load_with_deployment(
            project.path(),
            first.path(),
            Some(selection.clone()),
        )
        .unwrap();
        assert_eq!(
            frozen.runtime_stdlib(),
            selection.sources.root(NativeSourceRole::Stdlib)
        );
        assert_eq!(
            frozen.runtime_actors(),
            selection.sources.root(NativeSourceRole::Actors)
        );
        assert!(!first.path().join("workspace/libraries").exists());
        let identical = FrozenWorkspace::load_with_deployment(
            project.path(),
            second.path(),
            Some(selection.clone()),
        )
        .unwrap();
        assert_eq!(frozen.identity(), identical.identity());
        let resumed =
            FrozenWorkspace::load_with_deployment(project.path(), first.path(), Some(selection))
                .unwrap();
        assert_eq!(resumed.identity(), frozen.identity());
    }

    #[test]
    fn deployed_stdlib_changed_source_refuses_without_rewriting_selection() {
        let (_fixture, selection) = deployment_fixture();
        let project = deployment_project("[defaults]\nmodel = 'gpt-6-sol'\n");
        let run = tempfile::tempdir().unwrap();
        FrozenWorkspace::load_with_deployment(project.path(), run.path(), Some(selection.clone()))
            .unwrap();
        let manifest = run.path().join("workspace/selection.json");
        let admitted = std::fs::read(&manifest).unwrap();
        let prelude = selection
            .sources
            .root(NativeSourceRole::Stdlib)
            .join("Tidepool/Prelude.hs");
        let original = std::fs::read(&prelude).unwrap();
        std::fs::write(&prelude, "module Tidepool.Prelude where\n").unwrap();
        assert!(FrozenWorkspace::load_with_deployment(
            project.path(),
            run.path(),
            Some(selection.clone())
        )
        .is_err());
        std::fs::write(&prelude, original).unwrap();
        let extra = selection
            .sources
            .root(NativeSourceRole::Stdlib)
            .join("Tidepool/DeploymentExtra.hs");
        std::fs::write(&extra, "module Tidepool.DeploymentExtra where\n").unwrap();
        assert!(FrozenWorkspace::load_with_deployment(
            project.path(),
            run.path(),
            Some(selection.clone())
        )
        .is_err());
        std::fs::remove_file(extra).unwrap();
        std::fs::remove_file(prelude).unwrap();
        assert!(
            FrozenWorkspace::load_with_deployment(project.path(), run.path(), Some(selection))
                .is_err()
        );
        assert_eq!(std::fs::read(manifest).unwrap(), admitted);
    }

    #[test]
    fn deployed_actor_changes_and_aliases_refuse_without_copying_sources() {
        let (fixture, selection) = deployment_fixture();
        let project = deployment_project("[defaults]\nmodel = 'gpt-6-sol'\n");
        let run = tempfile::tempdir().unwrap();
        FrozenWorkspace::load_with_deployment(project.path(), run.path(), Some(selection.clone()))
            .unwrap();
        let manifest = run.path().join("workspace/selection.json");
        let admitted = std::fs::read(&manifest).unwrap();
        let actors = selection.sources.root(NativeSourceRole::Actors);
        let sentinel = actors.join("Tidepool/Check.hs");
        let original = std::fs::read(&sentinel).unwrap();
        std::fs::write(&sentinel, "changed actor source").unwrap();
        assert!(FrozenWorkspace::load_with_deployment(
            project.path(),
            run.path(),
            Some(selection.clone())
        )
        .is_err());
        std::fs::write(&sentinel, original).unwrap();
        std::fs::rename(&actors, fixture.path().join("original-actors")).unwrap();
        std::os::unix::fs::symlink(fixture.path().join("original-actors"), &actors).unwrap();
        assert!(
            FrozenWorkspace::load_with_deployment(project.path(), run.path(), Some(selection))
                .is_err()
        );
        assert_eq!(std::fs::read(manifest).unwrap(), admitted);
        assert!(!run.path().join("workspace/libraries").exists());
    }

    #[test]
    fn deployed_stdlib_changed_authority_or_format_refuses_resume() {
        let (_fixture, selection) = deployment_fixture();
        let project = deployment_project("[defaults]\nmodel = 'gpt-6-sol'\n");
        let run = tempfile::tempdir().unwrap();
        FrozenWorkspace::load_with_deployment(project.path(), run.path(), Some(selection.clone()))
            .unwrap();
        let manifest = run.path().join("workspace/selection.json");
        let admitted = std::fs::read(&manifest).unwrap();
        let mut changed = vec![selection.clone(); 7];
        changed[0].version = 2;
        changed[1].source_pin.push_str("-changed");
        changed[2].producer_identity[0] ^= 1;
        changed[3].catalog_identity.push_str("-changed");
        changed[4].sources.snapshot_root = selection
            .sources
            .snapshot_root
            .parent()
            .unwrap()
            .join("relocated");
        changed[5].sources.roles.swap(0, 1);
        changed[6].sources.source_files[0]
            .sha256
            .push_str("-changed");
        for selection in changed {
            assert!(FrozenWorkspace::load_with_deployment(
                project.path(),
                run.path(),
                Some(selection)
            )
            .is_err());
            assert_eq!(std::fs::read(&manifest).unwrap(), admitted);
        }
        assert!(FrozenWorkspace::load_with_deployment(project.path(), run.path(), None).is_err());
        let mut old: serde_json::Value = serde_json::from_slice(&admitted).unwrap();
        old["version"] = serde_json::json!(2);
        let old = serde_json::to_vec(&old).unwrap();
        std::fs::write(&manifest, &old).unwrap();
        let error =
            FrozenWorkspace::load_with_deployment(project.path(), run.path(), Some(selection))
                .unwrap_err();
        assert!(error
            .to_string()
            .contains("unsupported frozen workspace format"));
        assert_eq!(std::fs::read(manifest).unwrap(), old);
    }

    #[test]
    fn deployed_stdlib_preserves_ordered_authored_source_selection() {
        let (_fixture, selection) = deployment_fixture();
        let project = deployment_project(
            "[defaults]\nmodel = 'gpt-6-sol'\n[haskell]\nsource_roots = ['first', 'second']\n",
        );
        for (root, value) in [("first", 1), ("second", 2)] {
            let path = project.path().join(".exomonad").join(root).join("Project");
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(
                path.join("Shared.hs"),
                format!("module Project.Shared where\nvalue = {value}\n"),
            )
            .unwrap();
        }
        let run = tempfile::tempdir().unwrap();
        let frozen =
            FrozenWorkspace::load_with_deployment(project.path(), run.path(), Some(selection))
                .unwrap();
        let roots = frozen.captured_source_roots();
        assert_eq!(roots.len(), 2);
        let winner = roots
            .iter()
            .find_map(|root| std::fs::read_to_string(root.join("Project/Shared.hs")).ok())
            .unwrap();
        assert!(winner.contains("value = 1"));
        assert!(std::fs::read_to_string(roots[1].join("Project/Shared.hs"))
            .unwrap()
            .contains("value = 2"));
        assert!(frozen.include.last().unwrap().ends_with("resources"));
    }

    #[test]
    fn runtime_libraries_are_captured_and_changed_sources_refuse_publication() {
        let root = tempfile::tempdir().unwrap();
        let selected = crate::haskell_sources::runtime_source_roots(None).unwrap();
        let relative = [PathBuf::from("stdlib"), PathBuf::from("actors")];
        for (source, path) in selected.iter().zip(&relative) {
            capture_sources(source, path, root.path(), &mut BTreeMap::new()).unwrap();
        }
        let sources = relative.map(|path| root.path().join(path));
        let prelude = sources[0].join("Tidepool/Prelude.hs");
        let original = std::fs::read_to_string(&prelude).unwrap();
        let expected = tidepool_toolchain::cache::source_roots_identity(
            crate::haskell_sources::DEV_SOURCE_DOMAIN,
            &sources,
        )
        .unwrap();
        let destination = root.path().join("run");
        let captured = capture_runtime_libraries(
            &sources,
            &expected,
            &destination,
            uuid::Uuid::new_v4(),
            &mut BTreeMap::new(),
        )
        .unwrap();
        std::fs::write(prelude, format!("{original}\n-- changed after capture\n")).unwrap();
        assert_eq!(
            std::fs::read_to_string(captured[0].join("Tidepool/Prelude.hs")).unwrap(),
            original
        );
        let refused = capture_runtime_libraries(
            &sources,
            &expected,
            &destination,
            uuid::Uuid::new_v4(),
            &mut BTreeMap::new(),
        )
        .unwrap_err();
        assert_eq!(
            refused.to_string(),
            "runtime Haskell library changed during run capture"
        );
    }

    #[test]
    fn frozen_runtime_library_mutation_fails_host_load() {
        let project = tempfile::tempdir().unwrap();
        let run = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(project.path().join(".exomonad")).unwrap();
        std::fs::write(
            project.path().join(".exomonad/config.toml"),
            "[defaults]\nmodel = 'gpt-6-sol'\n",
        )
        .unwrap();
        let selected = FrozenWorkspace::load(project.path(), run.path()).unwrap();
        std::fs::write(
            selected.runtime_stdlib().join("Tidepool/Prelude.hs"),
            "mutated",
        )
        .unwrap();
        assert!(FrozenWorkspace::load(project.path(), run.path()).is_err());
    }

    #[test]
    fn frozen_spec_selection_accepts_null_tools_and_rejects_obsolete_tools() {
        let project = tempfile::tempdir().unwrap();
        let run = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(project.path().join(".exomonad")).unwrap();
        std::fs::write(
            project.path().join(".exomonad/config.toml"),
            "[defaults]\nmodel = 'gpt-6-sol'\n[haskell]\nspec = 'Project.Spec.agentSpec'\n",
        )
        .unwrap();
        let selected = FrozenWorkspace::load(project.path(), run.path()).unwrap();
        let manifest = run.path().join("workspace/selection.json");
        let mut selection: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
        assert_eq!(selection["version"], 6);
        selection["tools"] = serde_json::Value::Null;
        std::fs::write(&manifest, serde_json::to_vec(&selection).unwrap()).unwrap();
        let retained = FrozenWorkspace::load(project.path(), run.path()).unwrap();
        assert_eq!(retained.spec, selected.spec);
        assert_eq!(retained.identity, selected.identity);
        selection["tools"] = serde_json::json!("Project.Tools.tools");
        std::fs::write(&manifest, serde_json::to_vec(&selection).unwrap()).unwrap();
        let error = FrozenWorkspace::load(project.path(), run.path())
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("frozen workspace uses obsolete [haskell] tools"),
            "{error}"
        );
        assert!(error.contains("then start a new run"), "{error}");
    }

    #[test]
    fn pre_capture_selection_requires_explicit_new_run() {
        let project = tempfile::tempdir().unwrap();
        let run = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(project.path().join(".exomonad")).unwrap();
        std::fs::write(
            project.path().join(".exomonad/config.toml"),
            "[defaults]\nmodel = 'gpt-6-sol'\n",
        )
        .unwrap();
        FrozenWorkspace::load(project.path(), run.path()).unwrap();
        let manifest = run.path().join("workspace/selection.json");
        let mut selection: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
        selection["version"] = serde_json::json!(1);
        selection
            .as_object_mut()
            .unwrap()
            .remove("runtime_libraries");
        selection.as_object_mut().unwrap().remove("runtime_actors");
        selection
            .as_object_mut()
            .unwrap()
            .remove("runtime_capture_identity");
        selection.as_object_mut().unwrap().remove("core_identity");
        std::fs::write(&manifest, serde_json::to_vec(&selection).unwrap()).unwrap();
        let error = FrozenWorkspace::load(project.path(), run.path()).unwrap_err();
        assert!(error
            .to_string()
            .contains("unsupported frozen workspace format"));
    }

    #[test]
    fn generated_core_identity_must_match_on_host_load() {
        let project = tempfile::tempdir().unwrap();
        let run = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(project.path().join(".exomonad")).unwrap();
        std::fs::write(
            project.path().join(".exomonad/config.toml"),
            "[defaults]\nmodel = 'gpt-6-sol'\n",
        )
        .unwrap();
        FrozenWorkspace::load(project.path(), run.path()).unwrap();
        let manifest = run.path().join("workspace/selection.json");
        let mut selection: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
        selection["core_identity"] = serde_json::json!("different-core");
        std::fs::write(&manifest, serde_json::to_vec(&selection).unwrap()).unwrap();
        let error = FrozenWorkspace::load(project.path(), run.path()).unwrap_err();
        assert!(error.to_string().contains("generated Core differs"));
    }

    #[test]
    fn selection_freezes_dependencies_prompts_and_configuration_until_next_run() {
        let project = tempfile::tempdir().unwrap();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let authored = project.path().join(".exomonad");
        std::fs::create_dir_all(authored.join("Project")).unwrap();
        std::fs::write(authored.join("config.toml"), "[defaults]\nmodel = 'gpt-6-sol'\n[haskell]\nsource_roots = ['.']\nmodules = ['Project.Work']\n[prompts]\ncore = 'core.md'\n").unwrap();
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
    fn canonical_workspace_contrib_sources_are_captured() {
        let project = tempfile::tempdir().unwrap();
        let run = tempfile::tempdir().unwrap();
        let authored = project.path().join(".exomonad");
        let contrib = authored.join("Exomonad/Contrib");
        std::fs::create_dir_all(&contrib).unwrap();
        std::fs::write(
            authored.join("config.toml"),
            "[defaults]\nmodel = 'gpt-6-sol'\n[haskell]\nsource_roots = ['.']\n",
        )
        .unwrap();
        let canonical_module = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../exomonad/examples/workspace/.exomonad/Exomonad/Contrib/Types.hs");
        std::fs::copy(&canonical_module, contrib.join("Types.hs")).unwrap();

        let selected = FrozenWorkspace::load(project.path(), run.path()).unwrap();

        let captured = selected.include[0].join("Exomonad/Contrib/Types.hs");
        assert_eq!(
            std::fs::read(captured).unwrap(),
            std::fs::read(canonical_module).unwrap()
        );
    }

    #[test]
    fn generated_workspace_module_path_is_reserved_for_each_source_suffix() {
        for suffix in ["hs", "lhs", "hs-boot", "lhs-boot"] {
            let project = tempfile::tempdir().unwrap();
            let run = tempfile::tempdir().unwrap();
            let authored = project.path().join(".exomonad");
            let collision = authored.join(format!("Exomonad/Workspace.{suffix}"));
            std::fs::create_dir_all(collision.parent().unwrap()).unwrap();
            std::fs::write(&collision, "").unwrap();
            std::fs::write(
                authored.join("config.toml"),
                "[defaults]\nmodel = 'gpt-6-sol'\n[haskell]\nsource_roots = ['.']\n",
            )
            .unwrap();

            let error = FrozenWorkspace::load(project.path(), run.path())
                .unwrap_err()
                .to_string();
            assert!(error.contains("Exomonad.Workspace is reserved"), "{error}");
        }
    }

    #[test]
    fn invalid_import_or_missing_source_fails_before_launch() {
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir(project.path().join(".exomonad")).unwrap();
        for module in ["Project.Work\nimport Bad", "Project.Missing"] {
            let run = tempfile::tempdir().unwrap();
            std::fs::write(
                project.path().join(".exomonad/config.toml"),
                format!(
                    "[defaults]\nmodel = 'gpt-6-sol'\n[haskell]\nmodules = [{}]\n",
                    serde_json::to_string(module).unwrap()
                ),
            )
            .unwrap();
            assert!(FrozenWorkspace::load(project.path(), run.path()).is_err());
        }
    }

    #[test]
    fn obsolete_tools_key_explains_the_spec_migration() {
        for tools in ["'Project.Tools.tools'", "17", "[]"] {
            let config = format!("tools = {tools}\nspec = 'Project.Spec.agentSpec'\n");
            let error = toml::from_str::<HaskellConfig>(&config)
                .unwrap_err()
                .to_string();
            assert!(error.contains("[haskell] tools is obsolete"), "{error}");
            assert!(error.contains("specTools = yourTools"), "{error}");
        }
    }

    #[test]
    fn spec_selection_is_qualified_and_frozen_with_the_package() {
        let project = tempfile::tempdir().unwrap();
        let authored = project.path().join(".exomonad");
        std::fs::create_dir(&authored).unwrap();
        let write = |entry: &str| {
            std::fs::write(
                authored.join("config.toml"),
                format!(
                    "[defaults]\nmodel='gpt-6-sol'\n[haskell]\nspec={}\n",
                    serde_json::to_string(entry).unwrap(),
                ),
            )
            .unwrap()
        };
        for entry in [
            "agentSpec",
            "Project.Spec.agentSpec\nimport Bad",
            "Project.Spec.AgentSpec",
            "Project.Spec.agentSpec ()",
        ] {
            write(entry);
            assert!(
                FrozenWorkspace::load(project.path(), tempfile::tempdir().unwrap().path()).is_err()
            );
        }
        write("Project.Spec.agentSpec");
        let run = tempfile::tempdir().unwrap();
        let frozen = FrozenWorkspace::load(project.path(), run.path()).unwrap();
        assert_eq!(frozen.spec.as_deref(), Some("Project.Spec.agentSpec"));
        write("Project.Next.agentSpec");
        let same_run = FrozenWorkspace::load(project.path(), run.path()).unwrap();
        assert_eq!(same_run.spec, frozen.spec);
        let next =
            FrozenWorkspace::load(project.path(), tempfile::tempdir().unwrap().path()).unwrap();
        assert_eq!(next.spec.as_deref(), Some("Project.Next.agentSpec"));
        assert_ne!(next.identity, frozen.identity);
    }

    /// External Haskell source pinned through the project's flake reaches a
    /// run the same way authored source does: captured into the frozen
    /// workspace, importable by module name, and covered by the compile
    /// cache's own content fingerprint. A local override stands in for the pin
    /// without rewriting `flake.lock`, and editing that override moves the
    /// cache key — which is what makes the rapid-development loop honest.
    #[test]
    fn flake_pinned_sources_are_captured_overridden_and_rekeyed() {
        let project = tempfile::tempdir().unwrap();
        let pinned = tempfile::tempdir().unwrap();
        let authored = project.path().join(".exomonad");
        std::fs::create_dir_all(&authored).unwrap();
        let local = authored.join("local-ext");
        write_tiny_module(pinned.path(), "41");
        write_tiny_module(&local, "99");
        pin_tiny_input(project.path(), pinned.path());
        let config = |overridden: bool| {
            let mut text = String::from(
                "[defaults]\nmodel = 'gpt-6-sol'\n[haskell]\nmodules = ['Ext.Tiny']\n[haskell.flake_sources]\ntiny = ['src']\n",
            );
            if overridden {
                text.push_str("[haskell.flake_overrides]\ntiny = 'local-ext'\n");
            }
            std::fs::write(authored.join("config.toml"), text).unwrap();
        };
        let tiny = |frozen: &FrozenWorkspace| {
            let root = frozen
                .include
                .iter()
                .find(|root| root.join("Ext/Tiny.hs").is_file())
                .expect("the pinned module is captured as an ordinary source root");
            std::fs::read_to_string(root.join("Ext/Tiny.hs")).unwrap()
        };

        config(false);
        let first_run = tempfile::tempdir().unwrap();
        let first = FrozenWorkspace::load(project.path(), first_run.path()).unwrap();
        assert!(tiny(&first).contains("41"), "{}", tiny(&first));
        let lock = std::fs::read(project.path().join("flake.lock")).unwrap();

        config(true);
        let second_run = tempfile::tempdir().unwrap();
        let second = FrozenWorkspace::load(project.path(), second_run.path()).unwrap();
        assert!(tiny(&second).contains("99"), "{}", tiny(&second));
        assert_eq!(
            std::fs::read(project.path().join("flake.lock")).unwrap(),
            lock,
            "an override is a working change, not a re-pin"
        );
        assert_ne!(first.identity, second.identity);
        assert_ne!(cache_key(&first.include), cache_key(&second.include));

        write_tiny_module(&local, "123");
        let third_run = tempfile::tempdir().unwrap();
        let third = FrozenWorkspace::load(project.path(), third_run.path()).unwrap();
        assert!(tiny(&third).contains("123"), "{}", tiny(&third));
        assert_ne!(cache_key(&second.include), cache_key(&third.include));
    }

    #[test]
    fn unavailable_flake_input_refuses_offline_source_admission() {
        assert!(
            std::env::var_os(super::super::ENV_NIX_OFFLINE).is_some(),
            "native source admission must explicitly refuse network fetching"
        );
        assert!(
            nix_bin().is_file(),
            "source admission requires its declared Nix executable"
        );
        let project = tempfile::tempdir().unwrap();
        let missing = project.path().join("unavailable-pinned-input");
        pin_tiny_input(project.path(), &missing);
        let config = HaskellConfig {
            flake_sources: BTreeMap::from([("tiny".to_owned(), vec![PathBuf::from("src")])]),
            ..HaskellConfig::default()
        };
        let error = resolve_source_roots(project.path(), &config)
            .expect_err("missing pinned source must refuse")
            .to_string();
        assert!(
            error.contains("cannot fetch the project's flake inputs"),
            "{error}"
        );
        assert!(error.contains("unavailable-pinned-input"), "{error}");
        assert!(!project.path().join("flake.lock").exists());
    }

    /// The pinned module is not merely captured. The resident machine compiles
    /// it into the swarm's own program, every actor reaches it through the same
    /// include list, and a prepared cell evaluates a value that only the
    /// external source can supply.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_flake_pinned_module_answers_a_prepared_cell() {
        let project = tempfile::tempdir().unwrap();
        let pinned = tempfile::tempdir().unwrap();
        let authored = project.path().join(".exomonad/Project");
        std::fs::create_dir_all(&authored).unwrap();
        write_tiny_module(pinned.path(), "41");
        std::fs::write(
            project.path().join(".exomonad/config.toml"),
            "[defaults]\nmodel = 'gpt-6-sol'\n[haskell]\nsource_roots = ['.']\nmodules = ['Ext.Tiny']\nchecks = ['Project.Checks.pinned']\n[haskell.flake_sources]\ntiny = ['src']\n",
        )
        .unwrap();
        std::fs::write(
            authored.join("Checks.hs"),
            include_str!("workspace_pinned_check.hs"),
        )
        .unwrap();
        pin_tiny_input(project.path(), pinned.path());
        crate::exomonad::check(Some(project.path().to_path_buf()), true)
            .await
            .unwrap();
    }

    fn write_tiny_module(root: &Path, value: &str) {
        std::fs::create_dir_all(root.join("src/Ext")).unwrap();
        std::fs::write(
            root.join("src/Ext/Tiny.hs"),
            format!("module Ext.Tiny where\n\ntiny :: Int\ntiny = {value}\n"),
        )
        .unwrap();
    }

    /// Declare `tiny` as a non-flake source input of the project's own flake.
    /// Nix resolves a project directory through its enclosing Git tree, which
    /// every Exomonad workspace has, and reads only files Git knows about.
    fn pin_tiny_input(project: &Path, pinned: &Path) {
        std::fs::write(
            project.join("flake.nix"),
            format!(
                "{{\n  inputs.tiny = {{ url = \"path:{}\"; flake = false; }};\n  outputs = _: {{ }};\n}}\n",
                pinned.display()
            ),
        )
        .unwrap();
        for argv in [&["init", "-q"][..], &["add", "-A"][..]] {
            assert!(std::process::Command::new("git")
                .args(argv)
                .current_dir(project)
                .status()
                .unwrap()
                .success());
        }
    }

    /// Source-revision identity; artifact reuse additionally validates the
    /// compiler's consumed dependency and import-resolution evidence.
    fn cache_key(include: &[PathBuf]) -> String {
        tidepool_toolchain::cache::source_roots_identity(b"source-revision-test", include).unwrap()
    }

    #[test]
    fn workspace_program_validation_uses_frozen_sources_and_rejects_bad_revisions() {
        let project = tempfile::tempdir().unwrap();
        let old_run = tempfile::tempdir().unwrap();
        let new_run = tempfile::tempdir().unwrap();
        let authored = project.path().join(".exomonad");
        std::fs::create_dir_all(authored.join("Project")).unwrap();
        std::fs::write(
            authored.join("config.toml"),
            "[defaults]\nmodel = 'gpt-6-sol'\n[haskell]\nsource_roots = ['.']\nmodules = ['Project.Check']\n",
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

#[cfg(test)]
mod source_capture_tests;
