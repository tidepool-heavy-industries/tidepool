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
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    pub(crate) async fn run(self) -> Result<SrReloadOutcome, SourceError> {
        let active = self
            .layers
            .revision(self.expected.installation.source())
            .map_err(|error| match error {
                crate::SourceInspectionError::Unavailable(detail) => {
                    SourceError::SourceUnavailable(detail)
                }
                crate::SourceInspectionError::Unreadable(detail) => {
                    SourceError::SourceUnreadable(detail)
                }
            })?;
        if self
            .control
            .as_ref()
            .is_some_and(|control| control.cancellation_requested())
        {
            return Ok(SrReloadOutcome::ReloadCancelled(active));
        }
        let requested_checks = !self.checked.is_empty();
        let layers = self.layers.clone();
        let actor = self.expected.context.actor;
        let source = self.expected.installation.source().clone();
        let staged = tokio::task::spawn_blocking(move || {
            layers.stage_reload(actor.into(), &source, &self.checked)
        })
        .await
        .map_err(unreadable)?
        .map_err(|outcome| match outcome {
            crate::SourceLayerReload::Unavailable(detail) => SourceError::SourceUnavailable(detail),
            other => unreadable(format!("candidate capture refused: {other:?}")),
        })?;
        let previous = staged.previous_revision().clone();
        let candidate_revision = staged.candidate_revision().clone();
        let configuration_unchanged =
            staged.configuration_digest() == self.expected.configuration_digest;
        if configuration_unchanged && !requested_checks {
            return Ok(SrReloadOutcome::ReloadUnchanged(previous));
        }
        let (context, authority) = WorkbenchCompilationAuthority::admit(
            self.expected.context.clone(),
            staged.source().clone(),
            None,
            Some(&self.layers),
        )
        .map_err(unreadable)?;
        let workbench = self.workbench.with_compilation_authority(authority);
        let install = self
            .expected
            .installation
            .tools()
            .ok_or_else(|| {
                SourceError::SourceUnavailable(
                    "actor has no source-defined tool installation".into(),
                )
            })?
            .install
            .checked_add(1)
            .ok_or_else(|| {
                SourceError::SourceUnavailable("tool installation counter exhausted".into())
            })?;
        let candidate = match workbench
            .prepare_candidate_tools(
                context.clone(),
                install,
                self.descriptor.capabilities().effect_keys().to_vec(),
                staged.validation_imports().clone(),
            )
            .await
        {
            Ok(candidate) => candidate,
            Err(ResidentActorWorkbenchError::InvocationCancelled) => {
                return Ok(SrReloadOutcome::ReloadCancelled(previous));
            }
            Err(error)
                if error.failure_diagnostic().is_some_and(|diagnostic| {
                    diagnostic.class == tidepool_toolchain::failclass::FailureClass::UserHaskell
                }) =>
            {
                return Ok(SrReloadOutcome::ReloadRejected(
                    previous,
                    candidate_revision,
                    error.to_string(),
                ));
            }
            Err(error) => return Err(unreadable(error)),
        };
        let changes = exomonad_tool::surface::compare_surfaces(
            &self
                .expected
                .installation
                .tools()
                .expect("selected source installation")
                .surface(),
            &candidate.surface(),
        );
        if !changes.is_empty() {
            return Ok(SrReloadOutcome::ReloadRejected(
                previous,
                candidate_revision,
                format!(
                    "registered tool surface changed; a new actor incarnation is required:\n{}",
                    exomonad_tool::surface::describe_changes(&changes)
                ),
            ));
        }
        if configuration_unchanged {
            return Ok(SrReloadOutcome::ReloadUnchanged(previous));
        }
        let installation = crate::InstalledToolLease::new(
            actor,
            staged.source().clone(),
            Some(Arc::new(candidate)),
        );
        let prompts = staged.prompt_snapshot().clone();
        let references = installation
            .generation_references(staged.source_reference().clone(), prompts.clone())
            .map_err(unreadable)?;
        let prepared = workbench
            .prepare_actor_generation(
                context.clone(),
                self.descriptor.clone(),
                Some(self.expected.clone()),
                references,
                installation.clone(),
            )
            .await
            .map_err(unreadable)?;
        let publication = self.control.as_ref().map_or_else(
            tidepool_runtime::session::PublicationDecision::new,
            |control| control.publication_decision(),
        );
        let result = workbench
            .commit_actor_generation(
                context,
                self.descriptor,
                self.state,
                ActorRequestSnapshot::Generation(self.expected),
                prepared,
                installation,
                prompts,
                staged.configuration_digest(),
                publication,
                if self.control.is_some() {
                    crate::resident_workbench::ActorGenerationPublication::Continuing
                } else {
                    crate::resident_workbench::ActorGenerationPublication::Terminal
                },
            )
            .await
            .map_err(unreadable)?;
        match result {
            ActorGenerationCommit::Published {
                generation,
                cancellation_pending,
            } => {
                if let Some(control) = self.control.as_ref() {
                    control.complete_continuing_publication(cancellation_pending);
                }
                if let Some(lifecycle) = self.lifecycle {
                    lifecycle.generation_published(generation);
                }
                Ok(SrReloadOutcome::ReloadPublished(
                    previous,
                    candidate_revision,
                    staged.changed_modules().to_vec(),
                ))
            }
            ActorGenerationCommit::Refused(_detail)
                if self
                    .control
                    .as_ref()
                    .is_some_and(|control| control.cancellation_requested()) =>
            {
                Ok(SrReloadOutcome::ReloadCancelled(previous))
            }
            ActorGenerationCommit::Refused(detail) => Err(SourceError::SourceUnavailable(detail)),
            ActorGenerationCommit::Unconfirmed(detail) => Ok(
                SrReloadOutcome::ReloadPublicationUnconfirmed(candidate_revision, detail),
            ),
        }
    }
}

impl<H, O> ResidentKernelBehavior<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    pub(crate) fn coherent_reload_operation(
        &self,
        generation: Arc<ActorGeneration>,
        control: Option<Arc<crate::WorkbenchExecutionControl>>,
        checked: Vec<String>,
    ) -> Result<CoherentReloadOperation<H, O>, SourceError> {
        let active = self.installed_tools.current_generation().ok_or_else(|| {
            SourceError::SourceUnavailable("actor generation admission is unavailable".into())
        })?;
        if !Arc::ptr_eq(&active, &generation) {
            return Err(SourceError::SourceUnavailable("issued generation is no longer current; an already accepted computation remains pinned".into()));
        }
        let tools = generation.installation.tools().ok_or_else(|| {
            SourceError::SourceUnavailable("actor has no source-defined tool installation".into())
        })?;
        if tools.resolved_spec().is_none() {
            return Err(SourceError::SourceUnavailable(
                "explicit live AgentSpec implementations have no editable source-backed reload"
                    .into(),
            ));
        }
        Ok(CoherentReloadOperation {
            workbench: self
                .environment
                .runner
                .application_workbench()
                .with_intrinsic_effect_support(self.environment.intrinsic_effect_support()),
            layers: self.environment.source_layers.clone().ok_or_else(|| {
                SourceError::SourceUnavailable("actor has no editable source owner".into())
            })?,
            descriptor: self.descriptor.clone(),
            state: self.installed_tools.clone(),
            expected: generation,
            control,
            checked,
            lifecycle: self.lifecycle.clone(),
        })
    }

    pub(super) fn owned_reload_task(
        &self,
        owned: OwnedExecution<H, O>,
    ) -> OwnedWorkbenchTask<Self> {
        let operation = crate::reload_tool::parse(
            owned
                .state
                .request
                .tool_call()
                .expect("native reload")
                .arguments
                .clone(),
        )
        .map_err(unreadable)
        .and_then(|arguments| {
            let generation = owned
                .state
                .effects
                .issued_snapshot
                .as_ref()
                .and_then(ActorRequestSnapshot::generation)
                .cloned()
                .ok_or_else(|| {
                    SourceError::SourceUnavailable(
                        "reload requires an initialized notebook generation".into(),
                    )
                })?;
            self.coherent_reload_operation(
                generation,
                owned.state.effects.control.clone(),
                arguments.also_check,
            )
        });
        Self::owned_step_task(
            owned,
            move |_owned| {
                Box::pin(async move {
                    match operation {
                        Ok(operation) => operation.run().await,
                        Err(error) => Err(error),
                    }
                })
            },
            move |behavior, _kernel, owned, result| {
                if matches!(
                    result,
                    Ok(SrReloadOutcome::ReloadPublished(..)
                        | SrReloadOutcome::ReloadPublishedWithWarnings(..))
                ) {
                    behavior.spec_installs = behavior
                        .installed_tools
                        .current_tools()
                        .map_or(behavior.spec_installs, |tools| tools.install);
                }
                Ok(WorkbenchAdvance::Park({
                    let execution = owned
                        .state
                        .request
                        .execution_id()
                        .cloned()
                        .expect("native reload execution identity");
                    Self::finish_owned_task(owned, reload_result(result, execution))
                }))
            },
        )
    }
}

fn revision_value(revision: &tidepool_bridge_effects::SrRevision) -> serde_json::Value {
    // Native receipts summarize the revision; source inspection and Haskell
    // reload results retain its complete module inventory.
    serde_json::json!({"identity": revision.identity, "generation": revision.generation,
        "moduleCount": revision.modules.len()})
}

fn reload_result(
    result: Result<SrReloadOutcome, SourceError>,
    execution: tidepool_runtime::session::WorkbenchExecutionId,
) -> Result<KernelStep<WorkbenchResponse>, WorkbenchExecutionFailure> {
    let disposition = source_effect::reload_disposition(&result);
    let (output, value, warnings) = match result {
        Ok(SrReloadOutcome::ReloadUnchanged(revision)) => (
            format!(
                "unchanged: source {}; notebook names retained",
                revision.identity
            ),
            serde_json::json!({"outcome":"unchanged","revision":revision_value(&revision)}),
            Vec::new(),
        ),
        Ok(SrReloadOutcome::ReloadPublished(previous, next, changed)) => (
            format!(
                "published: source {}; compatible handlers and prompts active; notebook reset",
                next.identity
            ),
            serde_json::json!({"outcome":"published","previous":revision_value(&previous),"revision":revision_value(&next),"changedModules":changed}),
            Vec::new(),
        ),
        Ok(SrReloadOutcome::ReloadPublishedWithWarnings(previous, next, changed, warnings)) => (
            format!(
                "published: source {}; notebook reset; derived projections need repair",
                next.identity
            ),
            serde_json::json!({"outcome":"publishedWithWarnings","previous":revision_value(&previous),"revision":revision_value(&next),"changedModules":changed}),
            warnings,
        ),
        Ok(SrReloadOutcome::ReloadRejected(issued, candidate, diagnostics)) => (
            format!(
                "rejected: candidate {}; reload changed no active configuration\n{}",
                candidate.identity, diagnostics
            ),
            serde_json::json!({"outcome":"rejected","issuedRevision":revision_value(&issued),"candidate":revision_value(&candidate),"diagnostics":diagnostics}),
            Vec::new(),
        ),
        Ok(SrReloadOutcome::ReloadCancelled(issued)) => (
            "cancelled before publication; reload changed no active configuration".into(),
            serde_json::json!({"outcome":"cancelled","issuedRevision":revision_value(&issued)}),
            Vec::new(),
        ),
        Ok(SrReloadOutcome::ReloadPublicationUnconfirmed(candidate, diagnostics)) => (
            format!(
                "publication unconfirmed: candidate {}; admission blocked; reconcile before continuing\n{}",
                candidate.identity, diagnostics
            ),
            serde_json::json!({"outcome":"publicationUnconfirmed","candidate":revision_value(&candidate),"diagnostics":diagnostics}),
            Vec::new(),
        ),
        Err(SourceError::SourceUnavailable(detail)) => (
            format!("unavailable: {detail}"),
            serde_json::json!({"outcome":"unavailable","detail":detail}),
            Vec::new(),
        ),
        Err(SourceError::SourceUnreadable(detail)) => (
            format!("source unreadable: {detail}"),
            serde_json::json!({"outcome":"unreadable","detail":detail}),
            Vec::new(),
        ),
    };
    let (run_status, item_status, completed, layer) = match disposition {
        WorkbenchOperationDisposition::Read | WorkbenchOperationDisposition::Committed => (
            WorkbenchRunStatus::Committed,
            WorkbenchItemStatus::Committed,
            1,
            None,
        ),
        WorkbenchOperationDisposition::Rejected => (
            WorkbenchRunStatus::Rejected,
            WorkbenchItemStatus::Rejected,
            0,
            Some(WorkbenchFailureLayer::Effect),
        ),
        WorkbenchOperationDisposition::Unknown => (
            WorkbenchRunStatus::Rejected,
            WorkbenchItemStatus::Stopped,
            0,
            Some(WorkbenchFailureLayer::Effect),
        ),
        _ => unreachable!("reload result owns a terminal publication disposition"),
    };
    Ok(KernelStep::Continue(workbench_response(
        run_status,
        vec![WorkbenchItemReceipt {
            diagnostics: Vec::new(),
            index: 0,
            kind: None,
            span: None,
            source_items: Vec::new(),
            status: item_status,
            output,
            value: Some(value),
            warnings,
            installed_bindings: Vec::new(),
            operations: vec![WorkbenchOperationReceipt {
                id: WorkbenchOperationId {
                    execution,
                    input_unit_index: 0,
                    effect_ordinal: 0,
                },
                effect: "configuration reload".into(),
                disposition,
                display: None,
                display_publication: None,
            }],
            terminal_transfer: None,
            failure_layer: layer,
        }],
        completed,
        1,
        None,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn revision_with_modules(identity: &str, count: usize) -> tidepool_bridge_effects::SrRevision {
        tidepool_bridge_effects::SrRevision {
            generation: 1,
            identity: identity.into(),
            modules: (0..count)
                .map(|index| tidepool_bridge_effects::SrModule {
                    name: format!("Unchanged.Inventory.Module{index}"),
                    digest: format!("digest-{index:064}"),
                })
                .collect(),
        }
    }

    fn revision(identity: &str) -> tidepool_bridge_effects::SrRevision {
        revision_with_modules(identity, 393)
    }
    #[test]
    fn native_reload_receipts_preserve_each_typed_publication_outcome() {
        let cases = [
            (
                Ok(SrReloadOutcome::ReloadUnchanged(revision("A"))),
                WorkbenchOperationDisposition::Read,
                WorkbenchItemStatus::Committed,
                "unchanged",
            ),
            (
                Ok(SrReloadOutcome::ReloadPublished(
                    revision("A"),
                    revision("B"),
                    vec!["Work".into()],
                )),
                WorkbenchOperationDisposition::Committed,
                WorkbenchItemStatus::Committed,
                "published",
            ),
            (
                Ok(SrReloadOutcome::ReloadPublishedWithWarnings(
                    revision("A"),
                    revision("B"),
                    Vec::new(),
                    vec!["projection pending".into()],
                )),
                WorkbenchOperationDisposition::Committed,
                WorkbenchItemStatus::Committed,
                "publishedWithWarnings",
            ),
            (
                Ok(SrReloadOutcome::ReloadRejected(
                    revision("A"),
                    revision("B"),
                    "invalid draft".into(),
                )),
                WorkbenchOperationDisposition::Rejected,
                WorkbenchItemStatus::Rejected,
                "rejected",
            ),
            (
                Ok(SrReloadOutcome::ReloadCancelled(revision("A"))),
                WorkbenchOperationDisposition::Rejected,
                WorkbenchItemStatus::Rejected,
                "cancelled",
            ),
            (
                Ok(SrReloadOutcome::ReloadPublicationUnconfirmed(
                    revision("B"),
                    "native acknowledgement missing".into(),
                )),
                WorkbenchOperationDisposition::Unknown,
                WorkbenchItemStatus::Stopped,
                "publicationUnconfirmed",
            ),
            (
                Err(SourceError::SourceUnavailable("not initialized".into())),
                WorkbenchOperationDisposition::Rejected,
                WorkbenchItemStatus::Rejected,
                "unavailable",
            ),
            (
                Err(SourceError::SourceUnreadable("permission denied".into())),
                WorkbenchOperationDisposition::Rejected,
                WorkbenchItemStatus::Rejected,
                "unreadable",
            ),
        ];
        for (answer, disposition, status, tag) in cases {
            let KernelStep::Continue(response) = reload_result(
                answer,
                tidepool_runtime::session::WorkbenchExecutionId::from_digest([0; 16]),
            )
            .unwrap_or_else(|_| panic!("native reload receipt conversion failed")) else {
                panic!("native reload returns a retained tool receipt")
            };
            assert_eq!(response.items[0].status, status, "{tag}");
            assert_eq!(
                response.items[0].operations[0].disposition, disposition,
                "{tag}"
            );
            assert_eq!(response.items[0].value.as_ref().unwrap()["outcome"], tag);
            let value = response.items[0].value.as_ref().unwrap();
            let summaries: &[(&str, &str)] = match tag {
                "unchanged" => &[("revision", "A")],
                "published" | "publishedWithWarnings" => &[("previous", "A"), ("revision", "B")],
                "rejected" => &[("issuedRevision", "A"), ("candidate", "B")],
                "cancelled" => &[("issuedRevision", "A")],
                "publicationUnconfirmed" => &[("candidate", "B")],
                "unavailable" | "unreadable" => &[],
                _ => panic!("unrecognized test outcome"),
            };
            for &(field, identity) in summaries {
                assert_eq!(value[field]["identity"], identity, "{tag}: {field}");
                assert_eq!(value[field]["generation"], 1, "{tag}: {field}");
                assert_eq!(value[field]["moduleCount"], 393, "{tag}: {field}");
                assert!(value[field].get("modules").is_none(), "{tag}: {field}");
            }
            let warnings = if tag == "publishedWithWarnings" {
                vec!["projection pending"]
            } else {
                Vec::new()
            };
            assert_eq!(response.items[0].warnings, warnings, "{tag}");
            match tag {
                "published" => assert_eq!(value["changedModules"], serde_json::json!(["Work"])),
                "publishedWithWarnings" => {
                    assert_eq!(value["changedModules"], serde_json::json!([]))
                }
                "rejected" => assert_eq!(value["diagnostics"], "invalid draft"),
                "publicationUnconfirmed" => {
                    assert_eq!(value["diagnostics"], "native acknowledgement missing")
                }
                "unavailable" => assert_eq!(value["detail"], "not initialized"),
                "unreadable" => assert_eq!(value["detail"], "permission denied"),
                _ => {}
            }
        }
    }

    #[test]
    fn native_reload_receipt_growth_depends_on_counts_not_module_inventories() {
        fn rendered(count: usize) -> serde_json::Value {
            let KernelStep::Continue(response) = reload_result(
                Ok(SrReloadOutcome::ReloadPublished(
                    revision_with_modules("A", count),
                    revision_with_modules("B", count),
                    vec!["Persona.SmokeHelper".into()],
                )),
                tidepool_runtime::session::WorkbenchExecutionId::from_digest([0; 16]),
            )
            .unwrap_or_else(|_| panic!("native reload receipt conversion failed")) else {
                panic!("native reload returns a retained tool receipt")
            };
            serde_json::to_value(response).unwrap()
        }
        let small = rendered(393);
        let large = rendered(3_930);
        for (rendered, count) in [(&small, 393), (&large, 3_930)] {
            let value = &rendered["items"][0]["value"];
            assert_eq!(value["previous"]["moduleCount"], count);
            assert_eq!(value["revision"]["moduleCount"], count);
            assert_eq!(
                value["changedModules"],
                serde_json::json!(["Persona.SmokeHelper"])
            );
            assert!(value["previous"].get("modules").is_none());
            assert!(value["revision"].get("modules").is_none());
        }
        // Each count adds one decimal digit. No inventory bytes may escape
        // through another field in the complete model-facing response.
        assert_eq!(
            serde_json::to_vec(&large).unwrap().len(),
            serde_json::to_vec(&small).unwrap().len() + 2
        );
        fn normalize_counts(mut rendered: serde_json::Value) -> serde_json::Value {
            for field in ["previous", "revision"] {
                rendered["items"][0]["value"][field]["moduleCount"] = serde_json::json!(0);
            }
            rendered
        }
        assert_eq!(normalize_counts(small), normalize_counts(large));
    }
}
