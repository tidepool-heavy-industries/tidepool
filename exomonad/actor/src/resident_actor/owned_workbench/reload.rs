//! Reload preparation owns its wait; the fenced completion installs its result.

use super::*;

#[derive(Clone, Copy)]
enum ReloadKind {
    Helpers,
    Spec,
}

struct SourcePreparation {
    outcome: &'static str,
    receipt: Vec<String>,
    source: Option<Result<crate::CheckpointSourceLayer, String>>,
    failure: Option<String>,
}

impl<H, O> ResidentKernelBehavior<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    pub(super) fn owned_reload_task(
        &self,
        owned: OwnedExecution<H, O>,
    ) -> OwnedWorkbenchTask<Self> {
        let call = owned.state.request.tool_call().expect("reload tool call");
        let (kind, checked) = match call.name.as_str() {
            crate::reload_helpers_tool::RELOAD_HELPERS_TOOL => (
                ReloadKind::Helpers,
                crate::reload_helpers_tool::parse(call.arguments.clone())
                    .map(|arguments| arguments.also_check)
                    .map_err(|error| error.to_string()),
            ),
            crate::reload_spec_tool::RELOAD_SPEC_TOOL => (
                ReloadKind::Spec,
                crate::reload_spec_tool::parse(call.arguments.clone())
                    .map(|arguments| arguments.also_check)
                    .map_err(|error| error.to_string()),
            ),
            _ => unreachable!("reload admission"),
        };
        let mut checked = match checked {
            Ok(checked) => checked,
            Err(error) => {
                return Self::finish_owned_task(
                    owned,
                    Err(workbench_failure(
                        &[],
                        0,
                        1,
                        ResidentActorWorkbenchError::ActorProtocol(error),
                    )),
                );
            }
        };
        let started = std::time::Instant::now();
        let mut receipt = Vec::new();
        if matches!(kind, ReloadKind::Spec) {
            let Some(expected) = self
                .installed_tools
                .current()
                .filter(|lease| lease.tools().is_some())
            else {
                return Self::finish_owned_task(
                    owned,
                    reload_result(
                        "this actor installed no agent spec, so there is nothing to reload.".into(),
                    ),
                );
            };
            let active = expected.tools().expect("selected installed spec");
            receipt.push(format!("spec: {}", active.origin.describe()));
            if active.origin.is_explicit() {
                receipt.push(
                    "filesystem spec reload refused: the actor uses an explicitly supplied live spec; use replaceSpec to replace its implementation.".into(),
                );
                return Self::finish_owned_task(
                    owned,
                    reload_result(reload_receipt("source unavailable", started, receipt)),
                );
            }
            if let Some(module) = active.origin.checked_module() {
                if !checked.contains(&module) {
                    checked.push(module);
                }
            }
            return self.owned_stage_spec_reload_task(owned, expected, checked, started, receipt);
        }
        let layers = self.environment.source_layers.clone();
        let actor = owned.state.effects.context.actor;
        Self::owned_step_task(
            owned,
            move |owned| {
                Box::pin(async move {
                    // The source owner combines checking and publication. Once it
                    // starts, retain the task until its actual outcome is known.
                    if owned
                        .state
                        .effects
                        .control
                        .as_ref()
                        .is_some_and(|control| control.cancellation_requested())
                    {
                        return Ok(SourcePreparation {
                            outcome: "cancelled",
                            receipt: vec!["reload cancelled before source publication.".into()],
                            source: None,
                            failure: None,
                        });
                    }
                    let publication = owned
                        .state
                        .effects
                        .control
                        .as_ref()
                        .expect("reload cancellation owner")
                        .publication_decision();
                    tidepool_runtime::spawn_blocking_in_span(move || {
                        prepare_helpers(layers, actor, checked, receipt, &publication)
                    })
                    .await
                    .map_err(|error| {
                        ResidentActorWorkbenchError::ActorProtocol(format!(
                            "reload source owner failed: {error}"
                        ))
                    })
                })
            },
            move |behavior, _kernel, owned, prepared| {
                let prepared = match prepared {
                    Ok(prepared) => prepared,
                    Err(error) => {
                        return Ok(WorkbenchAdvance::Park(Self::finish_owned_task(
                            owned,
                            Err(workbench_failure(&[], 0, 1, error)),
                        )))
                    }
                };
                let SourcePreparation {
                    outcome,
                    mut receipt,
                    source,
                    failure,
                } = prepared;
                match source {
                    Some(Ok(source)) => behavior
                        .installed_tools
                        .publish_source(owned.state.effects.context.actor, source),
                    Some(Err(error)) => {
                        behavior.installed_tools.clear();
                        receipt.push(format!("source: {error}"));
                        let result = match failure {
                            Some(failure) => Err(workbench_failure(
                                &[],
                                0,
                                1,
                                ResidentActorWorkbenchError::ActorProtocol(format!(
                                    "{failure}; frozen source unavailable: {error}"
                                )),
                            )),
                            None => reload_result(reload_receipt(
                                "source unavailable",
                                started,
                                receipt,
                            )),
                        };
                        return Ok(WorkbenchAdvance::Park(Self::finish_owned_task(
                            owned, result,
                        )));
                    }
                    None => {
                        return Ok(WorkbenchAdvance::Park(Self::finish_owned_task(
                            owned,
                            reload_result(reload_receipt(outcome, started, receipt)),
                        )))
                    }
                }
                if let Some(failure) = failure {
                    return Ok(WorkbenchAdvance::Park(Self::finish_owned_task(
                        owned,
                        Err(workbench_failure(
                            &[],
                            0,
                            1,
                            ResidentActorWorkbenchError::ActorProtocol(failure),
                        )),
                    )));
                }
                Ok(WorkbenchAdvance::Park(Self::finish_owned_task(
                    owned,
                    reload_result(reload_receipt(outcome, started, receipt)),
                )))
            },
        )
    }

    fn owned_stage_spec_reload_task(
        &self,
        owned: OwnedExecution<H, O>,
        expected: crate::InstalledToolLease,
        checked: Vec<String>,
        started: std::time::Instant,
        mut receipt: Vec<String>,
    ) -> OwnedWorkbenchTask<Self> {
        let layers = self.environment.source_layers.clone();
        let actor = owned.state.effects.context.actor;
        Self::owned_step_task(
            owned,
            move |owned| {
                Box::pin(async move {
                    if owned
                        .state
                        .effects
                        .control
                        .as_ref()
                        .is_some_and(|control| control.cancellation_requested())
                    {
                        return Ok(Err(crate::SourceLayerReload::Cancelled));
                    }
                    tidepool_runtime::spawn_blocking_in_span(move || {
                        let layers = layers.ok_or_else(|| {
                            crate::SourceLayerReload::Unavailable(
                                "this host installs no source layers".into(),
                            )
                        })?;
                        layers.stage_spec_reload(tidepool_repr::PrincipalId::from(actor), &checked)
                    })
                    .await
                    .map_err(|error| {
                        ResidentActorWorkbenchError::ActorProtocol(format!(
                            "reload source owner failed: {error}"
                        ))
                    })
                })
            },
            move |behavior, _kernel, owned, staged| match staged {
                Err(error) => Ok(WorkbenchAdvance::Park(Self::finish_owned_task(
                    owned,
                    Err(workbench_failure(&[], 0, 1, error)),
                ))),
                Ok(Err(outcome)) => {
                    let label = source_outcome_receipt(outcome, &mut receipt);
                    Ok(WorkbenchAdvance::Park(Self::finish_owned_task(
                        owned,
                        reload_result(reload_receipt(label, started, receipt)),
                    )))
                }
                Ok(Ok(staged)) => Ok(WorkbenchAdvance::Park(
                    behavior.owned_spec_reload_task(owned, expected, staged, started, receipt),
                )),
            },
        )
    }

    fn owned_spec_reload_task(
        &self,
        owned: OwnedExecution<H, O>,
        expected: crate::InstalledToolLease,
        staged: Box<dyn crate::StagedActorSourceReload>,
        started: std::time::Instant,
        mut receipt: Vec<String>,
    ) -> OwnedWorkbenchTask<Self> {
        let install = self.spec_installs + 1;
        let selected = WorkbenchCompilationAuthority::admit(
            owned.state.effects.context.clone(),
            staged.source().clone(),
            None,
            self.environment.source_layers.as_ref(),
        );
        let (context, authority) = match selected {
            Ok(selected) => selected,
            Err(error) => {
                return Self::finish_owned_task(
                    owned,
                    Err(workbench_failure(
                        &[],
                        0,
                        1,
                        ResidentActorWorkbenchError::ActorProtocol(error.to_string()),
                    )),
                )
            }
        };
        let workbench = self
            .environment
            .runner
            .application_workbench()
            .with_intrinsic_effect_support(self.environment.intrinsic_effect_support())
            .with_compilation_authority(authority);
        let granted_effects = self.descriptor.capabilities().effect_keys().to_vec();
        Self::owned_step_task(
            owned,
            move |owned| {
                Box::pin(async move {
                    if owned
                        .state
                        .effects
                        .control
                        .as_ref()
                        .is_some_and(|control| control.cancellation_requested())
                    {
                        return Ok(None);
                    }
                    workbench
                        .prepare_candidate_tools(context, install, granted_effects)
                        .await
                        .map(Some)
                })
            },
            move |behavior, _kernel, owned, candidate| {
                let candidate = match candidate {
                    Err(error) => {
                        receipt.push(format!("spec: preparation failed; previous source and handlers remain active.\n{error}"));
                        return Ok(WorkbenchAdvance::Park(Self::finish_owned_task(
                            owned,
                            reload_result(reload_receipt(
                                "spec preparation failed",
                                started,
                                receipt,
                            )),
                        )));
                    }
                    Ok(None) => {
                        receipt.push("reload cancelled before spec preparation; previous source and handlers remain active.".into());
                        return Ok(WorkbenchAdvance::Park(Self::finish_owned_task(
                            owned,
                            reload_result(reload_receipt("cancelled", started, receipt)),
                        )));
                    }
                    Ok(Some(candidate)) => candidate,
                };
                let active = expected
                    .tools()
                    .expect("reload retained its installed spec");
                let changes = exomonad_tool::surface::compare_surfaces(
                    &active.declarations,
                    &candidate.declarations,
                );
                if !changes.is_empty() {
                    receipt.push(format!("refused: the rebuilt spec declares a different registered surface; previous source and handlers remain active. A changed surface requires a new actor incarnation.\n{}", exomonad_tool::surface::describe_changes(&changes)));
                    return Ok(WorkbenchAdvance::Park(Self::finish_owned_task(
                        owned,
                        reload_result(reload_receipt("refused", started, receipt)),
                    )));
                }
                let provenance = candidate.provenance();
                let slots = candidate.slots.clone();
                let publication = owned
                    .state
                    .effects
                    .control
                    .as_ref()
                    .expect("reload publication owner")
                    .publication_decision();
                let outcome = behavior.installed_tools.commit_spec_reload(
                    &expected,
                    Arc::new(candidate),
                    staged,
                    &publication,
                );
                let visible = matches!(
                    &outcome,
                    crate::SourceLayerReload::Published { .. }
                        | crate::SourceLayerReload::PublicationUnconfirmed { .. }
                );
                if visible {
                    behavior.spec_installs = install;
                    behavior.after_tool.forget_failures();
                    receipt.push(format!("swapped: install {install} now serves later calls ({provenance}). A call already accepted keeps its original implementation."));
                    if !slots.is_empty() {
                        receipt.push(format!("slots: {}", slots.join(", ")));
                    }
                }
                let unconfirmed = match &outcome {
                    crate::SourceLayerReload::PublicationUnconfirmed { revision, diagnostics } => Some(format!("paired source revision {revision} and install {install} are visible but durability is unconfirmed: {diagnostics}")),
                    _ => None,
                };
                let label = source_outcome_receipt(outcome, &mut receipt);
                let result = match unconfirmed {
                    Some(detail) => Err(workbench_failure(
                        &[],
                        0,
                        1,
                        ResidentActorWorkbenchError::ActorProtocol(detail),
                    )),
                    None => reload_result(reload_receipt(
                        if visible { "swapped" } else { label },
                        started,
                        receipt,
                    )),
                };
                Ok(WorkbenchAdvance::Park(Self::finish_owned_task(
                    owned, result,
                )))
            },
        )
    }
}

fn prepare_helpers(
    layers: Option<crate::ActorSourceLayerResolver>,
    actor: ActorRef,
    checked: Vec<String>,
    mut receipt: Vec<String>,
    publication: &Arc<tidepool_runtime::session::PublicationDecision>,
) -> SourcePreparation {
    let label = "helpers";
    let Some(layers) = layers else {
        receipt.push(format!("{label}: this host installs no source layers."));
        return SourcePreparation {
            outcome: "unavailable",
            receipt,
            source: None,
            failure: None,
        };
    };
    let principal = tidepool_repr::PrincipalId::from(actor);
    let reloaded = layers.reload_helpers_with_publication(principal, &checked, publication);
    let outcome = match reloaded {
        crate::SourceLayerReload::Cancelled => {
            receipt.push(format!("{label}: cancelled before source publication."));
            return SourcePreparation {
                outcome: "cancelled",
                receipt,
                source: None,
                failure: None,
            };
        }
        crate::SourceLayerReload::PublicationUnconfirmed {
            revision,
            diagnostics,
        } => {
            return SourcePreparation { outcome: "publication unconfirmed", receipt,
                source: Some(layers.freeze_checkpoint_layer(principal)),
                failure: Some(format!("source revision {revision} is visible but durability is unconfirmed: {diagnostics}")),
            };
        }
        crate::SourceLayerReload::Unavailable(detail) => {
            receipt.push(format!("{label}: {detail}"));
            return SourcePreparation {
                outcome: "unavailable",
                receipt,
                source: None,
                failure: None,
            };
        }
        crate::SourceLayerReload::Rejected {
            active,
            rejected,
            diagnostics,
        } => {
            receipt.push(format!("{label}: rejected {rejected}; {active} remains active. Edited files remain on disk.\n{diagnostics}"));
            return SourcePreparation {
                outcome: "rejected",
                receipt,
                source: None,
                failure: None,
            };
        }
        crate::SourceLayerReload::Unchanged { revision } => {
            receipt.push(format!("{label}: unchanged at {revision}."));
            "unchanged"
        }
        crate::SourceLayerReload::Published {
            previous,
            revision,
            changed,
        } => {
            receipt.push(format!(
                "{label}: published {revision} over {previous}; changed {}.",
                if changed.is_empty() {
                    "nothing".into()
                } else {
                    changed.join(", ")
                }
            ));
            "published"
        }
    };
    let source = layers.freeze_checkpoint_layer(principal);
    SourcePreparation {
        outcome,
        receipt,
        source: Some(source),
        failure: None,
    }
}

fn source_outcome_receipt(
    outcome: crate::SourceLayerReload,
    receipt: &mut Vec<String>,
) -> &'static str {
    match outcome {
        crate::SourceLayerReload::Cancelled => {
            receipt.push("reload cancelled before paired publication; previous source and handlers remain active.".into());
            "cancelled"
        }
        crate::SourceLayerReload::Unavailable(detail) => {
            receipt.push(format!(
                "layer: {detail}; previous source and handlers remain active."
            ));
            "unavailable"
        }
        crate::SourceLayerReload::Rejected {
            active,
            rejected,
            diagnostics,
        } => {
            receipt.push(format!("layer: rejected {rejected}; {active} and previous handlers remain active. Edited files remain on disk.\n{diagnostics}"));
            "rejected"
        }
        crate::SourceLayerReload::Unchanged { revision } => {
            receipt.push(format!("layer: unchanged at {revision}."));
            "unchanged"
        }
        crate::SourceLayerReload::Published {
            previous,
            revision,
            changed,
        } => {
            receipt.push(format!(
                "layer: published {revision} over {previous}; changed {}.",
                changed.join(", ")
            ));
            "published"
        }
        crate::SourceLayerReload::PublicationUnconfirmed {
            revision,
            diagnostics,
        } => {
            receipt.push(format!("layer: {revision} and its new handlers are visible; durability unconfirmed: {diagnostics}"));
            "publication unconfirmed"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reload_receipt_leads_with_outcome_and_keeps_selected_revision_details() {
        let receipt = reload_receipt(
            "swapped",
            std::time::Instant::now(),
            vec!["spec: install 4 from helpers revision abc".into()],
        );
        assert!(receipt.starts_with("swapped ("));
        assert!(receipt.contains("spec: install 4 from helpers revision abc"));
    }
}

fn reload_result(
    output: String,
) -> Result<KernelStep<WorkbenchResponse>, WorkbenchExecutionFailure> {
    Ok(KernelStep::Continue(workbench_response(
        WorkbenchRunStatus::Committed,
        vec![WorkbenchItemReceipt {
            diagnostics: Vec::new(),
            index: 0,
            kind: None,
            span: None,
            source_items: Vec::new(),
            status: WorkbenchItemStatus::Committed,
            output,
            value: None,
            warnings: Vec::new(),
            installed_bindings: Vec::new(),
            operations: Vec::new(),
            terminal_transfer: None,
            failure_layer: None,
        }],
        1,
        1,
        None,
    )))
}
