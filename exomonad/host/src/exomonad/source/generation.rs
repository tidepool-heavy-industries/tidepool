//! Immutable inputs for an actor generation. Live selection belongs to the actor.

use super::*;
use exomonad_actor::{CheckpointSourceLayer, SourceGenerationEvidence, SourceInspectionError};
use tidepool_runtime::session::{
    ComposedPromptSnapshot, GenerationArtifactReference, SourceImports,
};

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceRootReference {
    path: PathBuf,
    files: Vec<(PathBuf, String)>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PromptReference {
    directory: PathBuf,
    files: Vec<(String, PathBuf, String)>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum EntryReference {
    Fresh {
        directory: PathBuf,
        preparation: uuid::Uuid,
    },
    Completed {
        directory: PathBuf,
        selections: BTreeMap<String, uuid::Uuid>,
    },
}

impl EntryReference {
    fn capture(entries: &exomonad_actor::SourceEntryStorage) -> Self {
        match entries {
            exomonad_actor::SourceEntryStorage::FreshCompilation {
                directory,
                preparation,
            } => Self::Fresh {
                directory: directory.clone(),
                preparation: *preparation,
            },
            exomonad_actor::SourceEntryStorage::CompletedOriginal {
                directory,
                selections,
            } => Self::Completed {
                directory: directory.clone(),
                selections: selections.clone(),
            },
        }
    }
    fn restore(self) -> exomonad_actor::SourceEntryStorage {
        match self {
            Self::Fresh {
                directory,
                preparation,
            } => exomonad_actor::SourceEntryStorage::FreshCompilation {
                directory,
                preparation,
            },
            Self::Completed {
                directory,
                selections,
            } => exomonad_actor::SourceEntryStorage::CompletedOriginal {
                directory,
                selections,
            },
        }
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceGraphReference {
    version: u32,
    domain: String,
    identities: Vec<String>,
    roots: Vec<SourceRootReference>,
    prompts: PromptReference,
    entries: EntryReference,
    revision: SourceRevision,
    helper_branch: String,
}

impl SourceGraphReference {
    fn configuration_digest(&self) -> Result<[u8; 32]> {
        let files = self
            .roots
            .iter()
            .map(|root| &root.files)
            .collect::<Vec<_>>();
        let bytes = serde_json::to_vec(&(
            "actor-generation-source-config-v1",
            &self.domain,
            &files,
            &self.prompts.files,
        ))?;
        Ok(*blake3::hash(&bytes).as_bytes())
    }
}

impl ExomonadSourceReload {
    pub(super) fn capture_prompts(
        &self,
        frozen: bool,
    ) -> Result<(CapturedPromptFiles, PromptReference)> {
        let config = self.frozen.config()?;
        let base = self.workspace.join(".exomonad");
        let mut selected = BTreeMap::new();
        for (name, path) in [
            ("base", config.prompts.base),
            ("agent", config.prompts.agent),
            ("api-guide", config.prompts.api_guide),
        ]
        .into_iter()
        .filter_map(|(name, path)| path.map(|path| (name.to_owned(), path)))
        .chain(config.prompts.files)
        {
            let absolute = base.join(&path);
            if absolute
                .ancestors()
                .any(|ancestor| ancestor == base.join("state"))
            {
                return Err("durable memory cannot be selected as a workspace prompt input".into());
            }
            let text = if frozen {
                self.frozen
                    .prompts
                    .get(&name)
                    .cloned()
                    .ok_or("configured prompt is absent from frozen inputs")?
            } else {
                if !std::fs::symlink_metadata(&absolute)?.file_type().is_file() {
                    return Err(format!(
                        "configured prompt is not a regular file: {}",
                        absolute.display()
                    )
                    .into());
                }
                std::fs::read_to_string(&absolute)?
            };
            if text.trim().is_empty() {
                return Err(format!("configured prompt {name:?} is empty").into());
            }
            selected.insert(name, (path, text));
        }
        let facts = selected
            .iter()
            .map(|(name, (path, text))| {
                (
                    name.clone(),
                    path.clone(),
                    blake3::hash(text.as_bytes()).to_hex().to_string(),
                )
            })
            .collect::<Vec<_>>();
        let identity = blake3::hash(&serde_json::to_vec(&facts)?)
            .to_hex()
            .to_string();
        let parent = self.run_root.join("workspace/prompt-revisions");
        std::fs::create_dir_all(&parent)?;
        let directory = parent.join(identity);
        if !directory.exists() {
            let staging = tempfile::Builder::new()
                .prefix(".pending-")
                .tempdir_in(&parent)?;
            for (index, (_, (_, text))) in selected.iter().enumerate() {
                tidepool_atomic_write::write_durable(
                    &staging.path().join(index.to_string()),
                    text.as_bytes(),
                )?;
            }
            sync_revision_tree(staging.path())?;
            std::fs::rename(staging.path(), &directory)?;
            let _retained = staging.keep();
            tidepool_atomic_write::sync_parent_directory(&directory)?;
        }
        let reference = PromptReference {
            directory,
            files: facts,
        };
        let captured = self.restore_prompts(&reference)?;
        Ok((captured, reference))
    }

    fn restore_prompts(&self, reference: &PromptReference) -> Result<CapturedPromptFiles> {
        let identity = blake3::hash(&serde_json::to_vec(&reference.files)?)
            .to_hex()
            .to_string();
        if reference.directory
            != self
                .run_root
                .join("workspace/prompt-revisions")
                .join(identity)
            || reference.directory.canonicalize()? != reference.directory
        {
            return Err("prompt reference is outside the retained run source owner".into());
        }
        if std::fs::read_dir(&reference.directory)?.count() != reference.files.len() {
            return Err("captured prompt directory has unreferenced files".into());
        }
        let mut files = BTreeMap::new();
        for (index, (name, _, expected)) in reference.files.iter().enumerate() {
            let path = reference.directory.join(index.to_string());
            if !std::fs::symlink_metadata(&path)?.file_type().is_file() {
                return Err("captured prompt is not a regular file".into());
            }
            let bytes = std::fs::read(path)?;
            if blake3::hash(&bytes).to_hex().as_str() != expected {
                return Err("captured prompt differs from its exact reference".into());
            }
            if files
                .insert(name.clone(), String::from_utf8(bytes)?)
                .is_some()
            {
                return Err("duplicate captured prompt role".into());
            }
        }
        Ok(CapturedPromptFiles { files })
    }

    pub(super) fn issue_graph(
        &self,
        identities: Vec<String>,
        include_paths: Vec<PathBuf>,
        manifests: Vec<tidepool_toolchain::cache::SourceRootManifest>,
        entries: exomonad_actor::SourceEntryStorage,
        prompts: PromptReference,
        helper_branch: &str,
    ) -> Result<CheckpointSourceLayer> {
        if include_paths.len() != manifests.len() {
            return Err("source graph has incomplete ordered root evidence".into());
        }
        let revision = source_revision(self.frozen.identity(), &manifests);
        let reference = SourceGraphReference {
            version: 2,
            domain: self.frozen.identity().into(),
            identities: identities.clone(),
            roots: include_paths
                .iter()
                .zip(&manifests)
                .map(|(path, manifest)| SourceRootReference {
                    path: path.clone(),
                    files: manifest
                        .files()
                        .map(|(path, hash)| (path.to_owned(), hash.to_hex().to_string()))
                        .collect(),
                })
                .collect(),
            prompts,
            entries: EntryReference::capture(&entries),
            revision,
            helper_branch: helper_branch.to_owned(),
        };
        let evidence = SourceGenerationEvidence {
            configuration_digest: reference.configuration_digest()?,
            reference: GenerationArtifactReference::capture(serde_json::to_vec(&reference)?),
        };
        Ok(self.source_issuer.issue(Arc::new(RetainedSourceGraph {
            _root_owner: Arc::clone(&self.source_owner),
            _prepared_owner: self.frozen.prepared_deployment.clone(),
            catalog: self.frozen.catalog_selection.clone(),
            identities,
            include_paths,
            manifests,
            entries,
            evidence,
        })))
    }

    pub(super) fn restore_graph(
        &self,
        reference: &GenerationArtifactReference,
    ) -> Result<CheckpointSourceLayer> {
        let source = self.restore_graph_owner(reference)?;
        Ok(self.source_issuer.issue(Arc::new(source)))
    }

    fn restore_graph_owner(
        &self,
        reference: &GenerationArtifactReference,
    ) -> Result<RetainedSourceGraph> {
        let graph: SourceGraphReference = serde_json::from_slice(reference.bytes())?;
        if graph.version != 2 || graph.domain != self.frozen.identity() {
            return Err("source graph reference belongs to another run selection".into());
        }
        let configuration_digest = graph.configuration_digest()?;
        self.restore_prompts(&graph.prompts)?;
        let mut include = Vec::new();
        let mut manifests = Vec::new();
        reference.validate()?;
        let mut expected_paths = Vec::new();
        let mut expected_identities = Vec::new();
        {
            let branch = &graph.helper_branch;
            if Path::new(branch).components().count() != 1
                || !matches!(
                    Path::new(branch).components().next(),
                    Some(std::path::Component::Normal(_))
                )
            {
                return Err("source reference carries an invalid helper branch".into());
            }
            let identity = graph
                .identities
                .first()
                .and_then(|identity| identity.strip_prefix("helpers:"))
                .ok_or("source reference lacks helper revision ownership")?;
            let layer = self.helper_layer(branch);
            let retained = if graph
                .roots
                .first()
                .is_some_and(|root| root.path.starts_with(layer.revisions()))
            {
                inspect_retained_revision(
                    self.frozen.identity(),
                    identity,
                    &layer.revisions().join(identity),
                    1,
                )?
            } else {
                self.prepared_helper_revision()?
                    .filter(|helper| helper.identity == identity)
                    .ok_or("source reference does not select this deployment's original helper")?
            };
            expected_identities.push(format!("helpers:{identity}"));
            expected_paths.extend(retained.paths);
        }
        let run_index = 1;
        let identity = graph
            .identities
            .get(run_index)
            .and_then(|identity| identity.strip_prefix("run:"))
            .ok_or("source reference lacks run revision ownership")?;
        let run_start = expected_paths.len();
        if graph
            .roots
            .get(run_start)
            .is_some_and(|root| root.path.starts_with(self.layer.revisions()))
        {
            expected_paths.extend(
                inspect_retained_revision(
                    self.frozen.identity(),
                    identity,
                    &self.layer.revisions().join(identity),
                    self.frozen.captured_source_roots().len(),
                )?
                .paths,
            );
        } else {
            let prepared = self
                .prepared_run_revision()?
                .filter(|prepared| prepared.identity == identity)
                .ok_or("source reference does not select this deployment's original source")?;
            expected_paths.extend(prepared.paths);
        }
        expected_identities.push(format!("run:{identity}"));
        expected_identities.push(format!("workspace:{}", self.frozen.identity()));
        expected_paths.push(self.frozen.workspace_resources().canonicalize()?);
        for root in self.shared_roots()? {
            if !expected_paths.contains(&root) {
                expected_paths.push(root);
            }
        }
        if graph.identities != expected_identities
            || graph
                .roots
                .iter()
                .map(|root| &root.path)
                .ne(expected_paths.iter())
        {
            return Err(
                "source reference does not contain the owner's complete ordered graph".into(),
            );
        }
        for root in graph.roots {
            if root.path.canonicalize()? != root.path {
                return Err("source reference carries an aliased root".into());
            }
            let actual = source_root_manifest(&root.path)?;
            let expected =
                tidepool_toolchain::cache::SourceRootManifest::from_file_digests(root.files)?;
            if !same_source_manifest(&actual, &expected) {
                return Err("immutable source root differs from its generation reference".into());
            }
            include.push(root.path);
            manifests.push(actual);
        }
        let entries = graph.entries.restore();
        if entries.directory() != self.fresh_entry_storage.directory()
            && entries.directory() != self.entry_storage.directory()
        {
            return Err("source reference selects unowned installer entry storage".into());
        }
        if source_revision(self.frozen.identity(), &manifests) != graph.revision {
            return Err("source revision receipt differs from its exact manifest facts".into());
        }
        let evidence = SourceGenerationEvidence {
            configuration_digest,
            reference: reference.clone(),
        };
        Ok(RetainedSourceGraph {
            _root_owner: Arc::clone(&self.source_owner),
            _prepared_owner: self.frozen.prepared_deployment.clone(),
            catalog: self.frozen.catalog_selection.clone(),
            identities: graph.identities,
            include_paths: include,
            manifests,
            entries,
            evidence,
        })
    }

    pub(super) fn restore_graph_with_entries(
        &self,
        reference: &GenerationArtifactReference,
        entries: exomonad_actor::SourceEntryStorage,
    ) -> Result<CheckpointSourceLayer> {
        let mut source = self.restore_graph_owner(reference)?;
        if entries.directory() != self.fresh_entry_storage.directory()
            && entries.directory() != self.entry_storage.directory()
        {
            return Err("source recovery selects unowned installer entry storage".into());
        }
        // Installer acquisition is a separate retained fact. Selecting its
        // completed original must not rewrite the committed source capsule.
        source.entries = entries;
        Ok(self.source_issuer.issue(Arc::new(source)))
    }

    pub(super) fn restore_graph_with_acquisition(
        &self,
        reference: &GenerationArtifactReference,
        acquisition: &exomonad_actor::ToolsetAcquisition,
    ) -> std::result::Result<CheckpointSourceLayer, SourceInspectionError> {
        let restore = || -> Result<CheckpointSourceLayer> {
            let mut source = self.restore_graph_owner(reference)?;
            let (storage, recipe, original) = match acquisition {
                exomonad_actor::ToolsetAcquisition::BuiltinDeploymentProgram { .. } => {
                    return Ok(self.source_issuer.issue(Arc::new(source)));
                }
                exomonad_actor::ToolsetAcquisition::DeploymentOriginal { recipe, original } => {
                    (&self.entry_storage, recipe, *original)
                }
                exomonad_actor::ToolsetAcquisition::FreshRunOriginal { recipe, original }
                | exomonad_actor::ToolsetAcquisition::ExistingRunOriginal { recipe, original } => {
                    (&self.fresh_entry_storage, recipe, *original)
                }
                exomonad_actor::ToolsetAcquisition::UnretainedCompilation { .. } => {
                    return Err("source recovery requires a retained installer acquisition".into());
                }
            };
            if recipe.len() != 64 || !recipe.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err("source recovery carries an invalid installer recipe selection".into());
            }
            let directory = storage.directory().to_owned();
            source.entries = exomonad_actor::SourceEntryStorage::CompletedOriginal {
                directory,
                selections: BTreeMap::from([(recipe.to_owned(), original)]),
            };
            Ok(self.source_issuer.issue(Arc::new(source)))
        };
        restore().map_err(|error| {
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
            {
                SourceInspectionError::Unavailable(error.to_string())
            } else {
                SourceInspectionError::Unreadable(error.to_string())
            }
        })
    }

    pub(super) fn shared_roots(&self) -> Result<Vec<PathBuf>> {
        let effects = tidepool_mcp::ensure_effects_module(
            &crate::actor_host::exomonad_effect_declarations(),
        )?;
        let mut roots = self.frozen.runtime_catalog_roots().unwrap_or_else(|| {
            vec![
                effects.core,
                self.frozen.runtime_actors(),
                self.frozen.runtime_stdlib(),
            ]
        });
        roots.push(self.frozen.runtime_orchestration());
        let mut selected = Vec::new();
        for root in roots {
            let root = root.canonicalize()?;
            if !selected.contains(&root) {
                selected.push(root);
            }
        }
        Ok(selected)
    }

    pub(super) fn append_shared_roots(
        &self,
        paths: &mut Vec<PathBuf>,
        manifests: &mut Vec<tidepool_toolchain::cache::SourceRootManifest>,
    ) -> Result<()> {
        for path in self.shared_roots()? {
            if !paths.contains(&path) {
                manifests.push(source_root_manifest(&path)?);
                paths.push(path);
            }
        }
        Ok(())
    }

    pub(super) fn stage_generation(
        self: Arc<Self>,
        actor: PrincipalId,
        expected: &CheckpointSourceLayer,
        also_check: &[String],
    ) -> std::result::Result<
        Box<dyn exomonad_actor::StagedActorSourceReload>,
        exomonad_actor::SourceLayerReload,
    > {
        use exomonad_actor::ActorSourceLayers;
        let unavailable = |error: Box<dyn std::error::Error>| {
            exomonad_actor::SourceLayerReload::Unavailable(error.to_string())
        };
        self.validate_source_authority(expected)
            .map_err(exomonad_actor::SourceLayerReload::Unavailable)?;
        let _gate = self.gate.lock();
        if !matches!(self.scope(actor), ActorSourceScope::Run) {
            return Err(exomonad_actor::SourceLayerReload::Unavailable(
                "captured or read-only actors cannot republish source".into(),
            ));
        }
        let helper_branch = self
            .helper_branch_for(actor)
            .unwrap_or_else(|| "run".into());
        let helper_layer = self.helper_layer(&helper_branch);
        let _helpers = helper_layer.lock_helpers().map_err(unavailable)?;
        let run = self
            .layer
            .capture_from_workspace(&self.frozen, &self.workspace)
            .map_err(unavailable)?;
        let helper = helper_layer
            .capture_from_roots(self.frozen.identity(), &[self.helper_draft(&helper_branch)])
            .map_err(unavailable)?;
        let helper_modules = helper
            .revision
            .modules
            .iter()
            .map(|(module, _)| module.clone())
            .collect::<BTreeSet<_>>();
        if helper_modules
            .iter()
            .any(|module| module != "SessionHelpers" && !module.starts_with("SessionHelpers."))
        {
            return Err(exomonad_actor::SourceLayerReload::Rejected {
                active: expected.identities().join("/"),
                rejected: helper.revision.identity.clone(),
                diagnostics: "helper modules must stay inside SessionHelpers".into(),
            });
        }
        if run
            .revision
            .modules
            .iter()
            .any(|(module, _)| helper_modules.contains(module))
        {
            return Err(exomonad_actor::SourceLayerReload::Rejected {
                active: expected.identities().join("/"),
                rejected: run.revision.identity.clone(),
                diagnostics: "run and helper source cannot both own a SessionHelpers module".into(),
            });
        }
        sync_revision_tree(&run.directory).map_err(unavailable)?;
        sync_revision_tree(&helper.directory).map_err(unavailable)?;
        let retained_run = inspect_retained_revision(
            self.frozen.identity(),
            &run.revision.identity,
            &run.directory,
            self.frozen.captured_source_roots().len(),
        )
        .map_err(unavailable)?;
        let retained_helper = inspect_retained_revision(
            self.frozen.identity(),
            &helper.revision.identity,
            &helper.directory,
            1,
        )
        .map_err(unavailable)?;
        let (prompts, prompt_reference) = self.capture_prompts(false).map_err(unavailable)?;
        let entries = exomonad_actor::SourceEntryStorage::FreshCompilation {
            directory: self.fresh_entry_storage.directory().to_owned(),
            preparation: uuid::Uuid::new_v4(),
        };
        let source = self
            .compose_graph(
                &helper_branch,
                retained_helper,
                retained_run,
                entries,
                prompt_reference,
            )
            .map_err(unavailable)?;
        let composer = self
            .prompt_composers
            .read()
            .get(&actor)
            .cloned()
            .ok_or_else(|| {
                exomonad_actor::SourceLayerReload::Unavailable(
                    "actor has no installed prompt compositor".into(),
                )
            })?;
        let prompt_snapshot =
            composer(&prompts).map_err(exomonad_actor::SourceLayerReload::Unavailable)?;
        let mut checked = self
            .frozen
            .modules
            .iter()
            .chain(&self.frozen.checks)
            .chain(also_check)
            .cloned()
            .collect::<BTreeSet<_>>();
        checked.extend(helper_modules);
        let old = manifest_modules(expected.source_manifests().ok_or_else(|| {
            exomonad_actor::SourceLayerReload::Unavailable(
                "expected source has no immutable manifest".into(),
            )
        })?);
        let new = manifest_modules(
            source
                .source_manifests()
                .expect("issued graph has manifests"),
        );
        checked.extend(
            new.iter()
                .filter(|(module, digest)| old.get(*module) != Some(*digest))
                .map(|(module, _)| module.clone()),
        );
        let validation_imports =
            validation_imports(checked).map_err(exomonad_actor::SourceLayerReload::Unavailable)?;
        let previous_revision = self.revision_issued(expected).map_err(|error| {
            exomonad_actor::SourceLayerReload::Unavailable(format!("{error:?}"))
        })?;
        let candidate_revision = self.revision_issued(&source).map_err(|error| {
            exomonad_actor::SourceLayerReload::Unavailable(format!("{error:?}"))
        })?;
        let changed_modules = old
            .keys()
            .chain(new.keys())
            .filter(|module| old.get(*module) != new.get(*module))
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        drop(_helpers);
        drop(_gate);
        Ok(Box::new(StagedGenerationSource {
            source,
            validation_imports,
            prompt_snapshot,
            previous_revision,
            candidate_revision,
            changed_modules,
        }))
    }

    pub(super) fn revision_issued(
        &self,
        source: &CheckpointSourceLayer,
    ) -> std::result::Result<tidepool_bridge_effects::SrRevision, SourceInspectionError> {
        use exomonad_actor::ActorSourceLayers;
        self.validate_source_authority(source)
            .map_err(SourceInspectionError::Unavailable)?;
        let evidence = source.generation_evidence().ok_or_else(|| {
            SourceInspectionError::Unavailable("source capsule has no generation evidence".into())
        })?;
        let graph: SourceGraphReference = serde_json::from_slice(evidence.reference.bytes())
            .map_err(|error| SourceInspectionError::Unreadable(error.to_string()))?;
        Ok(Self::wire(&graph.revision))
    }

    fn issued_module_path(
        &self,
        source: &CheckpointSourceLayer,
        module: &str,
    ) -> std::result::Result<Option<(PathBuf, blake3::Hash)>, SourceInspectionError> {
        use exomonad_actor::ActorSourceLayers;
        self.validate_source_authority(source)
            .map_err(SourceInspectionError::Unavailable)?;
        validate_module_name(module).map_err(SourceInspectionError::Unreadable)?;
        let manifests = source.source_manifests().ok_or_else(|| {
            SourceInspectionError::Unavailable("source capsule has no immutable manifest".into())
        })?;
        for (root, manifest) in source.include_paths().iter().zip(manifests) {
            for extension in ["hs", "lhs"] {
                let relative = PathBuf::from(module.replace('.', "/")).with_extension(extension);
                let Some((_, expected)) = manifest.files().find(|(path, _)| *path == relative)
                else {
                    continue;
                };
                return Ok(Some((root.join(relative), *expected)));
            }
        }
        Ok(None)
    }

    pub(super) fn locate_issued(
        &self,
        source: &CheckpointSourceLayer,
        module: &str,
    ) -> std::result::Result<Option<String>, SourceInspectionError> {
        self.issued_module_path(source, module)?
            .map(|_| {
                self.revision_issued(source)
                    .map(|revision| revision.identity)
            })
            .transpose()
    }

    pub(super) fn read_issued(
        &self,
        source: &CheckpointSourceLayer,
        module: &str,
    ) -> std::result::Result<(String, String), SourceInspectionError> {
        if let Some((path, expected)) = self.issued_module_path(source, module)? {
            let bytes = std::fs::read(&path).map_err(|error| {
                SourceInspectionError::Unreadable(format!("{}: {error}", path.display()))
            })?;
            if blake3::hash(&bytes) != expected {
                return Err(SourceInspectionError::Unreadable(
                    "issued immutable source changed".into(),
                ));
            }
            return Ok((
                self.revision_issued(source)?.identity,
                String::from_utf8(bytes)
                    .map_err(|error| SourceInspectionError::Unreadable(error.to_string()))?,
            ));
        }
        Err(SourceInspectionError::Unreadable(format!(
            "module {module} is absent from issued revision {}",
            self.revision_issued(source)?.identity
        )))
    }

    pub(super) fn observe_issued(
        &self,
        source: &CheckpointSourceLayer,
    ) -> std::result::Result<tidepool_bridge_effects::SrStatus, SourceInspectionError> {
        use exomonad_actor::ActorSourceLayers;
        self.validate_source_authority(source)
            .map_err(SourceInspectionError::Unavailable)?;
        let evidence = source.generation_evidence().ok_or_else(|| {
            SourceInspectionError::Unavailable("source capsule has no generation evidence".into())
        })?;
        let graph: SourceGraphReference = serde_json::from_slice(evidence.reference.bytes())
            .map_err(|error| SourceInspectionError::Unreadable(error.to_string()))?;
        let observed =
            (|| -> Result<SourceRevision> {
                let config = self.frozen.config()?;
                let run_roots = super::super::workspace::resolve_source_roots(
                    &self.workspace,
                    &config.haskell,
                )?;
                let mut manifests = Vec::new();
                let mut add =
                    |roots: &[PathBuf]| -> Result<()> {
                        let layer = roots.iter().map(|root| {
                    let files = super::super::workspace::inspect_sources(root)?;
                    Ok(tidepool_toolchain::cache::SourceRootManifest::from_file_digests(files)?)
                }).collect::<Result<Vec<_>>>()?;
                        let revision = source_revision(self.frozen.identity(), &layer);
                        manifests.extend(layer);
                        manifests.push(
                            tidepool_toolchain::cache::SourceRootManifest::from_file_digests([(
                                PathBuf::from(REVISION_MODULE),
                                blake3::hash(revision_module(&revision.identity).as_bytes())
                                    .to_hex()
                                    .to_string(),
                            )])?,
                        );
                        Ok(())
                    };
                add(&[self.helper_draft(&graph.helper_branch)])?;
                add(&run_roots)?;
                manifests.push(source_root_manifest(&self.frozen.workspace_resources())?);
                for root in self.shared_roots()? {
                    manifests.push(source_root_manifest(&root)?);
                }
                Ok(source_revision(self.frozen.identity(), &manifests))
            })()
            .map_err(|error| SourceInspectionError::Unreadable(error.to_string()))?;
        Ok(self.status_of(graph.revision, &observed))
    }
}

struct StagedGenerationSource {
    source: CheckpointSourceLayer,
    validation_imports: SourceImports,
    prompt_snapshot: ComposedPromptSnapshot,
    previous_revision: tidepool_bridge_effects::SrRevision,
    candidate_revision: tidepool_bridge_effects::SrRevision,
    changed_modules: Vec<String>,
}

impl exomonad_actor::StagedActorSourceReload for StagedGenerationSource {
    fn source(&self) -> &CheckpointSourceLayer {
        &self.source
    }
    fn evidence(&self) -> &SourceGenerationEvidence {
        self.source
            .generation_evidence()
            .expect("source owner issues complete generation evidence")
    }
    fn validation_imports(&self) -> &SourceImports {
        &self.validation_imports
    }
    fn prompt_snapshot(&self) -> &ComposedPromptSnapshot {
        &self.prompt_snapshot
    }
    fn previous_revision(&self) -> &tidepool_bridge_effects::SrRevision {
        &self.previous_revision
    }
    fn candidate_revision(&self) -> &tidepool_bridge_effects::SrRevision {
        &self.candidate_revision
    }
    fn changed_modules(&self) -> &[String] {
        &self.changed_modules
    }
}

fn manifest_modules(
    manifests: &[tidepool_toolchain::cache::SourceRootManifest],
) -> BTreeMap<String, String> {
    let mut modules = BTreeMap::new();
    for manifest in manifests {
        for (path, digest) in manifest.files() {
            if let Some(module) = module_name(path) {
                modules
                    .entry(module)
                    .or_insert_with(|| digest.to_hex().to_string());
            }
        }
    }
    modules
}

fn validate_module_name(module: &str) -> std::result::Result<(), String> {
    if module.split('.').all(|part| {
        part.chars()
            .next()
            .is_some_and(|first| first.is_ascii_uppercase())
            && part.chars().all(|character| {
                character.is_ascii_alphanumeric() || character == '_' || character == '\''
            })
    }) {
        Ok(())
    } else {
        Err(format!("invalid Haskell module name {module:?}"))
    }
}

fn validation_imports(modules: BTreeSet<String>) -> std::result::Result<SourceImports, String> {
    for module in &modules {
        validate_module_name(module)?;
    }
    Ok(SourceImports::from_specs(
        modules.into_iter().map(|module| format!("{module} ()")),
    ))
}
