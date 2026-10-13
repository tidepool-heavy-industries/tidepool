//! Reload preparation owns its wait; the fenced completion installs its result.

use super::*;

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
        let checked = crate::reload_tool::parse(call.arguments.clone())
            .map(|arguments| arguments.also_check)
            .map_err(|error| error.to_string());
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
        {
            let Some(expected) = self
                .installed_tools
                .current()
                .filter(|lease| lease.tools().is_some())
            else {
                let detail = match self.installed_tools.observe() {
                    crate::resident_workbench::InstalledToolsObservation::SourceUnavailable(_) =>
                        "the source installation is unavailable; repair it with reload_helpers before reloading the agent spec.",
                    _ => "this actor installed no agent spec, so there is nothing to reload.",
                };
                return Self::finish_owned_task(owned, reload_result(detail.into()));
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
                    owned
                        .spawn_blocking(move || {
                            let layers = layers.ok_or_else(|| {
                                crate::SourceLayerReload::Unavailable(
                                    "this host installs no source layers".into(),
                                )
                            })?;
                            layers.stage_spec_reload(
                                tidepool_repr::PrincipalId::from(actor),
                                &checked,
                            )
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
