//! Source-backed generation support only for the two generation-crossing cases.
use super::*;
use crate::resident_workbench::{
    ActorGenerationCommit, ActorGenerationPublication, BootstrapExpectation, HasReloadPreparation,
    InstalledToolsState, ReloadPreparation, ReloadPreparationOutcome, ReloadPreparationServices,
};
use crate::{ActorSourceLayers, SourceInspection};
use std::sync::atomic::{AtomicU64, Ordering};
use tidepool_runtime::session::{
    ComposedPromptSnapshot, GenerationArtifactReference, SourceImports,
};

struct FrozenSource {
    _root: Arc<tempfile::TempDir>,
    _entries: Arc<tempfile::TempDir>,
    paths: Vec<std::path::PathBuf>,
    identities: Vec<String>,
    entries: crate::SourceEntryStorage,
    evidence: crate::SourceGenerationEvidence,
}

impl crate::RetainedSourceLayer for FrozenSource {
    fn identities(&self) -> &[String] {
        &self.identities
    }
    fn include_paths(&self) -> &[std::path::PathBuf] {
        &self.paths
    }
    fn catalog_selection(&self) -> tidepool_toolchain::toolchain::CatalogSelection {
        tidepool_toolchain::toolchain::CatalogSelection::Acquired(None)
    }
    fn prepared_entries(&self) -> Option<&crate::SourceEntryStorage> {
        Some(&self.entries)
    }
    fn generation_evidence(&self) -> Option<&crate::SourceGenerationEvidence> {
        Some(&self.evidence)
    }
}

struct SourceOwner {
    actor: crate::ActorRef,
    issuer: crate::SourceLayerIssuer,
    root: Arc<tempfile::TempDir>,
    entries: Arc<tempfile::TempDir>,
    paths: Vec<std::path::PathBuf>,
    source_digest: String,
    preparation: uuid::Uuid,
    revision: AtomicU64,
}

impl SourceOwner {
    fn prompt(revision: u64) -> ComposedPromptSnapshot {
        ComposedPromptSnapshot::capture(format!("native provider generation {revision}"))
    }

    fn capture(&self, revision: u64) -> crate::CheckpointSourceLayer {
        let reference = GenerationArtifactReference::capture(
            serde_json::to_vec(&serde_json::json!({
                "module": "QuotedAgentSpec", "source_sha256": self.source_digest,
                "revision": revision, "prompts": Self::prompt(revision).instructions(),
            }))
            .unwrap(),
        );
        self.issuer.issue(Arc::new(FrozenSource {
            _root: self.root.clone(),
            _entries: self.entries.clone(),
            paths: self.paths.clone(),
            identities: vec![self.source_digest.clone()],
            entries: crate::SourceEntryStorage::FreshCompilation {
                directory: self.entries.path().to_owned(),
                preparation: self.preparation,
            },
            evidence: crate::SourceGenerationEvidence {
                configuration_digest: *reference.sha256(),
                reference,
            },
        }))
    }
}

impl SourceInspection for SourceOwner {
    fn revision(
        &self,
        source: &crate::CheckpointSourceLayer,
    ) -> Result<tidepool_bridge_effects::SrRevision, crate::SourceInspectionError> {
        self.validate_source_authority(source)
            .map_err(crate::SourceInspectionError::Unavailable)?;
        let reference = &source.generation_evidence().unwrap().reference;
        let data: serde_json::Value = serde_json::from_slice(reference.bytes()).unwrap();
        Ok(tidepool_bridge_effects::SrRevision {
            identity: reference
                .sha256()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
            generation: i64::try_from(data["revision"].as_u64().unwrap()).unwrap(),
            modules: Vec::new(),
        })
    }
}

struct StagedSource {
    source: crate::CheckpointSourceLayer,
    prompts: ComposedPromptSnapshot,
    previous: tidepool_bridge_effects::SrRevision,
    candidate: tidepool_bridge_effects::SrRevision,
    validation: SourceImports,
}

impl crate::StagedActorSourceReload for StagedSource {
    fn source(&self) -> &crate::CheckpointSourceLayer {
        &self.source
    }
    fn evidence(&self) -> &crate::SourceGenerationEvidence {
        self.source.generation_evidence().unwrap()
    }
    fn previous_revision(&self) -> &tidepool_bridge_effects::SrRevision {
        &self.previous
    }
    fn candidate_revision(&self) -> &tidepool_bridge_effects::SrRevision {
        &self.candidate
    }
    fn changed_modules(&self) -> &[String] {
        &[]
    }
    fn validation_imports(&self) -> &SourceImports {
        &self.validation
    }
    fn prompt_snapshot(&self) -> &ComposedPromptSnapshot {
        &self.prompts
    }
}

impl ActorSourceLayers for SourceOwner {
    fn inspection_services(self: Arc<Self>) -> crate::SourceInspectionServices {
        crate::SourceInspectionServices::new(self)
    }
    fn validate_source_authority(
        &self,
        source: &crate::CheckpointSourceLayer,
    ) -> Result<(), String> {
        self.issuer
            .owns(source)
            .then_some(())
            .ok_or_else(|| "foreign fixture source owner".into())
    }
    fn stage_reload(
        self: Arc<Self>,
        actor: tidepool_repr::PrincipalId,
        expected: &crate::CheckpointSourceLayer,
        _: &[String],
    ) -> Result<Box<dyn crate::StagedActorSourceReload>, crate::SourceLayerReload> {
        if actor != self.actor.into() {
            return Err(crate::SourceLayerReload::Unavailable(
                "foreign source actor".into(),
            ));
        }
        let previous = self
            .revision(expected)
            .map_err(|error| crate::SourceLayerReload::Unavailable(error.to_string()))?;
        let revision = self.revision.fetch_add(1, Ordering::SeqCst) + 1;
        let source = self.capture(revision);
        let candidate = self.revision(&source).unwrap();
        Ok(Box::new(StagedSource {
            source,
            prompts: Self::prompt(revision),
            previous,
            candidate,
            validation: SourceImports::default(),
        }))
    }
}

pub(super) struct GenerationSupport {
    runner: ResidentActorRunner<frunk::HNil, tidepool_mcp::CapturedOutput>,
    owner: Arc<SourceOwner>,
    quotation_executions: std::path::PathBuf,
    completed_executions: String,
}

impl GenerationSupport {
    pub(super) async fn install(
        forest: &ResidentForest<frunk::HNil, tidepool_mcp::CapturedOutput>,
        actor: &LocalActorRef,
        descriptor: &ActorDescriptor,
        state: &Arc<InstalledToolsState>,
    ) -> Self {
        let root = Arc::new(tempfile::tempdir().unwrap());
        let input = root.path().join("external-input");
        std::fs::write(&input, "41").unwrap();
        let text =
            tidepool_testing::fixture_source("exomonad/actor/src/fixtures/quoted-agent-spec.hs")
                .replace("{quotation-input}", input.to_str().unwrap());
        let provider = tidepool_testing::fixture_source(
            "exomonad/actor/src/fixtures/quoted-agent-provider.hs",
        );
        std::fs::write(root.path().join("QuotedAgentSpec.hs"), &text).unwrap();
        std::fs::write(root.path().join("QuotedProvider.hs"), &provider).unwrap();
        let effects = tidepool_mcp::ensure_effects_module(&[]).unwrap();
        let prelude = tidepool_testing::eval_harness::prelude_path()
            .canonicalize()
            .unwrap();
        let mut paths = effects.include_paths();
        paths.push(
            prelude
                .parent()
                .unwrap()
                .join("actors")
                .canonicalize()
                .unwrap(),
        );
        paths.push(prelude);
        paths.push(root.path().canonicalize().unwrap());
        let source_digest = GenerationArtifactReference::capture(
            serde_json::to_vec(&(&text, &provider, "41")).unwrap(),
        )
        .sha256()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
        let owner = Arc::new(SourceOwner {
            actor: actor.identity(),
            issuer: crate::SourceLayerIssuer::default(),
            root,
            paths: paths.clone(),
            entries: Arc::new(tempfile::tempdir().unwrap()),
            source_digest,
            preparation: uuid::Uuid::new_v4(),
            revision: AtomicU64::new(0),
        });
        let source = ActorWorkbenchSource::new("", paths).with_spec("QuotedAgentSpec.agentSpec");
        let runner = ResidentActorRunner::new(
            forest.environment.runner.machines_for_test().clone(),
            source,
        );
        let selected = owner.capture(0);
        let authority: crate::ActorSourceLayerResolver = owner.clone();
        let (context, compilation) = WorkbenchCompilationAuthority::admit(
            descriptor.session_context(actor.identity()),
            selected.clone(),
            None,
            Some(&authority),
        )
        .unwrap();
        let tools = actor
            .compiler_lifecycle_scope(
                runner
                    .application_workbench()
                    .with_compilation_authority(compilation)
                    .prepare_tools(context, 1, Vec::new()),
            )
            .await
            .unwrap();
        let expected = state.current().unwrap();
        state
            .install_bootstrap(
                BootstrapExpectation::Available(&expected),
                crate::InstalledToolLease::new(actor.identity(), selected, Some(Arc::new(tools))),
            )
            .unwrap();
        actor
            .lifecycle()
            .bootstrap_policy_installed(state.current());
        let quotation_executions = input.with_extension("executions");
        let completed_executions = std::fs::read_to_string(&quotation_executions).unwrap();
        assert!(!completed_executions.is_empty());
        Self {
            runner,
            owner,
            quotation_executions,
            completed_executions,
        }
    }

    pub(super) async fn publish_next(&self, fixture: &NativeFixture) {
        let snapshot = fixture.admission.lifecycle.resident_snapshot().unwrap();
        let workbench = self.runner.application_workbench();
        let prepared = if let Some(expected) = fixture.installed_tools.current_generation() {
            let services = ReloadPreparationServices::new(
                self.runner.clone(),
                self.owner.clone(),
                snapshot.descriptor,
                expected,
                None,
            );
            match fixture
                .actor
                .compiler_lifecycle_scope(ReloadPreparation::prepare(
                    services.reload_preparation(),
                    Vec::new(),
                    tidepool_bridge_effects::RlPolicy::CompatibleOnly,
                ))
                .await
                .unwrap()
            {
                ReloadPreparationOutcome::Ready(prepared) => prepared,
                ReloadPreparationOutcome::Settled(outcome) => {
                    panic!("expected actual successor: {outcome:?}")
                }
            }
        } else {
            let installation = fixture.installed_tools.current().unwrap();
            let context = snapshot
                .context
                .with_issued_source(installation.source())
                .unwrap();
            workbench
                .prepare_initial_generation(
                    fixture.installed_tools.clone(),
                    context,
                    snapshot.descriptor,
                    installation,
                    SourceOwner::prompt(0),
                )
                .await
                .unwrap()
        };
        let generation = match workbench
            .generation_publisher(fixture.installed_tools.clone())
            .publish(
                prepared,
                tidepool_runtime::session::PublicationDecision::new(),
                ActorGenerationPublication::Terminal,
            )
            .await
            .unwrap()
        {
            ActorGenerationCommit::Published { generation, .. } => generation,
            _ => panic!("real source-backed generation must publish"),
        };
        fixture.admission.lifecycle.generation_published(generation);
        assert_eq!(
            std::fs::read_to_string(&self.quotation_executions).unwrap(),
            self.completed_executions,
            "generation changes reuse the genuine completed original without another quotation"
        );
    }
}
