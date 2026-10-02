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
            let Some(active) = self.installed_tools.current_tools() else {
                return Self::finish_owned_task(
                    owned,
                    reload_result(
                        "this actor installed no agent spec, so there is nothing to reload.".into(),
                    ),
                );
            };
            receipt.push(format!("spec: {}", active.resolved.describe()));
            if let Some(module) = active.resolved.checked_module() {
                if !checked.contains(&module) {
                    checked.push(module);
                }
            }
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
                    tokio::task::spawn_blocking(move || {
                        prepare_source(layers, actor, kind, checked, receipt, &publication)
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
                match kind {
                    ReloadKind::Helpers => Ok(WorkbenchAdvance::Park(Self::finish_owned_task(
                        owned,
                        reload_result(reload_receipt(outcome, started, receipt)),
                    ))),
                    ReloadKind::Spec => Ok(WorkbenchAdvance::Park(
                        behavior.owned_spec_reload_task(owned, started, receipt),
                    )),
                }
            },
        )
    }

    fn owned_spec_reload_task(
        &self,
        owned: OwnedExecution<H, O>,
        started: std::time::Instant,
        mut receipt: Vec<String>,
    ) -> OwnedWorkbenchTask<Self> {
        let install = self.spec_installs + 1;
        let expected = self.installed_tools.current();
        let source = expected.as_ref().map(|lease| lease.source().clone());
        let selected = match source {
            Some(source) => WorkbenchCompilationAuthority::admit(
                owned.state.effects.context.clone(),
                source,
                expected.clone(),
                self.environment.source_layers.as_ref(),
            ),
            None => Err(KernelInvocationFailure::Rejected {
                actor: owned.state.effects.context.actor,
                detail: "the active source installation vanished before spec preparation".into(),
            }),
        };
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
            .with_compilation_authority(authority);
        Self::owned_step_task(
            owned,
            move |_owned| {
                Box::pin(async move {
                    let prepare_started = std::time::Instant::now();
                    let candidate = workbench.prepare_tools(context.clone(), install).await;
                    tracing::info!(actor = %context.actor, phase = "reload", install,
                    elapsed_ms = prepare_started.elapsed().as_millis(), success = candidate.is_ok(),
                    "agent spec preparation");
                    candidate
                })
            },
            move |behavior, _kernel, owned, candidate| {
                let outcome = match candidate {
                    Err(error) => {
                        receipt.push(format!("spec: the install fragment did not compile against the new revision, so the previous record is still active. The layer above WAS published, so your cells already see the edited modules.\n{error}"));
                        "spec did not compile"
                    }
                    Ok(candidate) => {
                        match behavior.installed_tools.current_tools() {
                            None => {
                                receipt.push("spec: the active record vanished mid-reload; nothing was swapped.".into());
                                "not swapped"
                            }
                            Some(active) => {
                                let current = behavior.installed_tools.current();
                                let installation_matches = current
                                    .as_ref()
                                    .zip(expected.as_ref())
                                    .is_some_and(|(current, expected)| {
                                        current.source() == expected.source()
                                            && current.tools().zip(expected.tools()).is_some_and(
                                                |(current, expected)| {
                                                    Arc::ptr_eq(
                                                        &current.dispatch,
                                                        &expected.dispatch,
                                                    )
                                                },
                                            )
                                    });
                                if !installation_matches {
                                    receipt.push("spec: the active source or installed record changed during preparation; nothing was swapped.".into());
                                    return Ok(WorkbenchAdvance::Park(Self::finish_owned_task(
                                        owned,
                                        reload_result(reload_receipt(
                                            "not swapped",
                                            started,
                                            receipt,
                                        )),
                                    )));
                                }
                                let changes = exomonad_tool::surface::compare_surfaces(
                                    &active.declarations,
                                    &candidate.declarations,
                                );
                                if changes.is_empty() {
                                    let decision = owned
                                        .state
                                        .effects
                                        .control
                                        .as_ref()
                                        .expect("reload publication owner")
                                        .publication_decision();
                                    let claim = if decision.phase()
                                        == tidepool_runtime::session::PublicationPhase::Published
                                    {
                                        None
                                    } else {
                                        match decision.claim_commit() {
                                        Some(claim) => Some(claim),
                                        None => return Ok(WorkbenchAdvance::Park(Self::finish_owned_task(owned,
                                            reload_result(reload_receipt("cancelled", started, vec!["reload cancelled before spec publication.".into()])),
                                        ))),
                                    }
                                    };
                                    receipt.push(format!("swapped: install {install} now serves later calls ({}). A call already accepted keeps the implementation it started with.", candidate.provenance()));
                                    if !candidate.slots.is_empty() {
                                        receipt
                                            .push(format!("slots: {}", candidate.slots.join(", ")));
                                    }
                                    behavior.spec_installs = install;
                                    behavior
                                        .installed_tools
                                        .publish_tools(Some(Arc::new(candidate)));
                                    behavior.after_tool.forget_failures();
                                    if let Some(claim) = claim {
                                        claim.published();
                                    }
                                    "swapped"
                                } else {
                                    receipt.push(format!("refused: the rebuilt spec declares a different surface, and the tool list was registered once for this session. The previous record is still serving calls; a changed surface takes effect at your next incarnation.\n{}", exomonad_tool::surface::describe_changes(&changes)));
                                    "refused"
                                }
                            }
                        }
                    }
                };
                Ok(WorkbenchAdvance::Park(Self::finish_owned_task(
                    owned,
                    reload_result(reload_receipt(outcome, started, receipt)),
                )))
            },
        )
    }
}

fn prepare_source(
    layers: Option<crate::ActorSourceLayerResolver>,
    actor: ActorRef,
    kind: ReloadKind,
    checked: Vec<String>,
    mut receipt: Vec<String>,
    publication: &Arc<tidepool_runtime::session::PublicationDecision>,
) -> SourcePreparation {
    let label = match kind {
        ReloadKind::Helpers => "helpers",
        ReloadKind::Spec => "layer",
    };
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
    let reloaded = match kind {
        ReloadKind::Helpers => {
            layers.reload_helpers_with_publication(principal, &checked, publication)
        }
        ReloadKind::Spec => layers.reload_with_publication(principal, &checked, publication),
    };
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
