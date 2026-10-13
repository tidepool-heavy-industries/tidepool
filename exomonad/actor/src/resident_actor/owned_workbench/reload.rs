//! One configuration operation serves the native tool and Haskell effect.
use super::*;
use crate::generated::source::SourceError;
use crate::resident_workbench::{ActorGeneration, ActorGenerationCommit, ActorRequestSnapshot};
use tidepool_bridge_effects::SrReloadOutcome;

pub(crate) struct CoherentReloadOperation<H, O> {
    workbench: crate::ResidentActorWorkbench<H, O>,
    layers: crate::ActorSourceLayerResolver,
    descriptor: crate::ActorDescriptor,
    state: Arc<crate::resident_workbench::InstalledToolsState>,
    expected: Arc<ActorGeneration>,
    control: Option<Arc<crate::WorkbenchExecutionControl>>,
    checked: Vec<String>,
    lifecycle: Option<crate::LifecycleHandle>,
}

fn unreadable(error: impl std::fmt::Display) -> SourceError {
    SourceError::SourceUnreadable(error.to_string())
}

impl<H, O> CoherentReloadOperation<H, O>
where H: DispatchEffect<O> + Send + 'static, O: OutputSink + Sync + 'static {
    pub(crate) async fn run(self) -> Result<SrReloadOutcome, SourceError> {
        let active = self.layers.revision(self.expected.installation.source()).map_err(|error| match error {
            crate::SourceInspectionError::Unavailable(detail) => SourceError::SourceUnavailable(detail),
            crate::SourceInspectionError::Unreadable(detail) => SourceError::SourceUnreadable(detail),
        })?;
        if self.control.as_ref().is_some_and(|control| control.cancellation_requested()) {
            return Ok(SrReloadOutcome::ReloadCancelled(active));
        }
        let layers = self.layers.clone();
        let actor = self.expected.context.actor;
        let source = self.expected.installation.source().clone();
        let staged = tokio::task::spawn_blocking(move || layers.stage_reload(actor.into(), &source, &self.checked))
            .await.map_err(unreadable)?.map_err(|outcome| match outcome {
                crate::SourceLayerReload::Unavailable(detail) => SourceError::SourceUnavailable(detail),
                other => unreadable(format!("candidate capture refused: {other:?}")),
            })?;
        let previous = staged.previous_revision().clone();
        let candidate_revision = staged.candidate_revision().clone();
        if staged.configuration_digest() == self.expected.configuration_digest {
            return Ok(SrReloadOutcome::ReloadUnchanged(previous));
        }
        let (context, authority) = WorkbenchCompilationAuthority::admit(
            self.expected.context.clone(), staged.source().clone(), None, Some(&self.layers),
        ).map_err(unreadable)?;
        let workbench = self.workbench.with_compilation_authority(authority);
        let install = self.expected.installation.tools().ok_or_else(|| SourceError::SourceUnavailable("actor has no source-defined tool installation".into()))?.install
            .checked_add(1).ok_or_else(|| SourceError::SourceUnavailable("tool installation counter exhausted".into()))?;
        let candidate = match workbench.prepare_candidate_tools(context.clone(), install,
            self.descriptor.capabilities().effect_keys().to_vec(), staged.validation_imports().clone()).await {
            Ok(candidate) => candidate,
            Err(ResidentActorWorkbenchError::InvocationCancelled) => return Ok(SrReloadOutcome::ReloadCancelled(previous)),
            Err(error) if error.failure_diagnostic().is_some_and(|diagnostic| diagnostic.class == tidepool_toolchain::failclass::FailureClass::UserHaskell) => {
                return Ok(SrReloadOutcome::ReloadRejected(previous,candidate_revision,error.to_string()));
            },
            Err(error) => return Err(unreadable(error)),
        };
        let changes = exomonad_tool::surface::compare_surfaces(
            &self.expected.installation.tools().expect("selected source installation").declarations,
            &candidate.declarations,
        );
        if !changes.is_empty() {
            return Ok(SrReloadOutcome::ReloadRejected(previous,candidate_revision,
                format!("registered tool surface changed; a new actor incarnation is required:\n{}",exomonad_tool::surface::describe_changes(&changes))));
        }
        let installation = crate::InstalledToolLease::new(actor,staged.source().clone(),Some(Arc::new(candidate)));
        let prompts = staged.prompt_snapshot().clone();
        let references = installation.generation_references(staged.source_reference().clone(),prompts.clone()).map_err(unreadable)?;
        let prepared = workbench.prepare_actor_generation(context.clone(),self.descriptor.clone(),Some(self.expected.clone()),references).await.map_err(unreadable)?;
        let publication = self.control.as_ref().map_or_else(tidepool_runtime::session::PublicationDecision::new,|control|control.publication_decision());
        let result = workbench.commit_actor_generation(context,self.descriptor,self.state,
            ActorRequestSnapshot::Generation(self.expected),prepared,installation,prompts,staged.configuration_digest(),publication).await.map_err(unreadable)?;
        match result {
            ActorGenerationCommit::Published(generation) => {
                if let Some(lifecycle) = self.lifecycle { lifecycle.generation_published(generation.context.clone(),generation.public_owner.clone(),generation.installation.clone()); }
                match staged.finalize_projection() {
                    Ok(()) => Ok(SrReloadOutcome::ReloadPublished(previous,candidate_revision,staged.changed_modules().to_vec())),
                    Err(warning) => Ok(SrReloadOutcome::ReloadPublishedWithWarnings(previous,candidate_revision,staged.changed_modules().to_vec(),vec![warning])),
                }
            },
            ActorGenerationCommit::Refused(_detail) if self.control.as_ref().is_some_and(|control|control.cancellation_requested()) => Ok(SrReloadOutcome::ReloadCancelled(previous)),
            ActorGenerationCommit::Refused(detail) => Err(SourceError::SourceUnavailable(detail)),
            ActorGenerationCommit::Unconfirmed(detail) => Ok(SrReloadOutcome::ReloadPublicationUnconfirmed(candidate_revision,detail)),
        }
    }
}

impl<H,O> ResidentKernelBehavior<H,O>
where H: DispatchEffect<O> + Send + 'static,O: OutputSink + Sync + 'static {
    pub(crate) fn coherent_reload_operation(&self,generation:Arc<ActorGeneration>,control:Option<Arc<crate::WorkbenchExecutionControl>>,checked:Vec<String>) -> Result<CoherentReloadOperation<H,O>,SourceError> {
        let active = self.installed_tools.current_generation().ok_or_else(|| SourceError::SourceUnavailable("actor generation admission is unavailable".into()))?;
        if !Arc::ptr_eq(&active,&generation) { return Err(SourceError::SourceUnavailable("issued generation is no longer current; an already accepted computation remains pinned".into())); }
        let tools = generation.installation.tools().ok_or_else(|| SourceError::SourceUnavailable("actor has no source-defined tool installation".into()))?;
        if tools.origin.is_explicit() { return Err(SourceError::SourceUnavailable("explicit live AgentSpec implementations have no editable source-backed reload".into())); }
        Ok(CoherentReloadOperation {
            workbench:self.environment.runner.application_workbench().with_intrinsic_effect_support(self.environment.intrinsic_effect_support()),
            layers:self.environment.source_layers.clone().ok_or_else(||SourceError::SourceUnavailable("actor has no editable source owner".into()))?,
            descriptor:self.descriptor.clone(),state:self.installed_tools.clone(),expected:generation,control,checked,lifecycle:self.lifecycle.clone(),
        })
    }

    pub(super) fn owned_reload_task(&self,owned:OwnedExecution<H,O>) -> OwnedWorkbenchTask<Self> {
        let operation = crate::reload_tool::parse(owned.state.request.tool_call().expect("native reload").arguments.clone())
            .map_err(unreadable).and_then(|arguments| {
                let generation = owned.state.effects.issued_snapshot.as_ref().and_then(ActorRequestSnapshot::generation).cloned()
                    .ok_or_else(|| SourceError::SourceUnavailable("reload requires an initialized notebook generation".into()))?;
                self.coherent_reload_operation(generation,owned.state.effects.control.clone(),arguments.also_check)
            });
        Self::owned_step_task(owned,move |_owned|Box::pin(async move {
            match operation {Ok(operation)=>operation.run().await,Err(error)=>Err(error)}
        }),move |behavior,_kernel,owned,result| {
            if matches!(result,Ok(SrReloadOutcome::ReloadPublished(..)|SrReloadOutcome::ReloadPublishedWithWarnings(..))) {
                behavior.spec_installs = behavior.installed_tools.current_tools().map_or(behavior.spec_installs,|tools|tools.install);
                behavior.after_tool.forget_failures();
            }
            Ok(WorkbenchAdvance::Park(Self::finish_owned_task(owned,reload_result(format!("{result:?}")))))
        })
    }
}

fn reload_result(output:String)->Result<KernelStep<WorkbenchResponse>,WorkbenchExecutionFailure> {
    Ok(KernelStep::Continue(workbench_response(WorkbenchRunStatus::Committed,vec![WorkbenchItemReceipt {
        diagnostics:Vec::new(),index:0,kind:None,span:None,source_items:Vec::new(),status:WorkbenchItemStatus::Committed,
        output,value:None,warnings:Vec::new(),installed_bindings:Vec::new(),operations:Vec::new(),terminal_transfer:None,failure_layer:None,
    }],1,1,None)))
}
