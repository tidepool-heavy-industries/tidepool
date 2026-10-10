use super::*;

struct ReplacementCandidate {
    placement: child_launch::ChildPlacementCustody,
    startup: Option<crate::WorkbenchAbandonGuard>,
    session_startup: Option<crate::resident_workbench::ChildSessionStartupLease>,
}

#[cfg(test)]
#[path = "replacement/refusal_tests.rs"]
mod tests;

impl ReplacementCandidate {
    fn new(placement: crate::ActorPlacement, predecessor: tidepool_repr::SessionId) -> Self {
        let placement = if placement.session == predecessor {
            child_launch::ChildPlacementCustody::new(placement)
        } else {
            child_launch::ChildPlacementCustody::reserved(placement)
        };
        let startup = Some(placement.startup_guard());
        Self {
            placement,
            startup,
            session_startup: None,
        }
    }
}

/// Staging owns only the new code's roots. The running actor still owns its
/// state, queued inputs, source connections and supervised resources.
struct StagedHandler {
    descriptor: ActorDescriptor,
    receiver: InstalledReceiver,
    checkpoint: StateCheckpoint,
    shutdown_hook: Option<RootCustody>,
    sources: Vec<crate::request::sources::SourceBinding>,
    dynamic_sources: Vec<crate::request::sources::SourceBinding>,
    session_startup: Option<crate::resident_workbench::ChildSessionStartupLease>,
    placement_custody: child_launch::ChildPlacementCustody,
    _placement_startup: Mutex<crate::WorkbenchAbandonGuard>,
    exit_destination: Option<Arc<crate::owned_result::RequestResultDestination>>,
}

pub(super) struct PreparedSuccessor {
    staged: StagedHandler,
    custody: tokio::sync::oneshot::Receiver<ReplacementCustody>,
}

/// Code and captured values can still reference a predecessor's resource scope. Keep
/// its roots and placement until the successor's resource owner retires them.
pub(super) struct RetainedHandler {
    pub(super) placement: crate::ActorPlacement,
    _standing: ResidentStanding,
    _checkpoint: Option<StateCheckpoint>,
    _sources: Vec<crate::request::sources::SourceBinding>,
    _shutdown_hook: Option<RootCustody>,
}

pub(super) struct ReplacementCustody {
    sources: Option<crate::request::sources::ActorSourceConnections>,
    worktree: Option<Arc<dyn crate::WorkspaceCustody>>,
    retained: Vec<RetainedHandler>,
}

pub(super) struct ReplacementTransfer {
    send: tokio::sync::oneshot::Sender<ReplacementCustody>,
}

impl<H, O> ResidentKernelBehavior<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    pub(super) async fn prepare_successor(
        &mut self,
        kernel: &KernelContext,
        definition: crate::ActorReplacementDefinition,
    ) -> Result<LocalActorRef, ResidentActorWorkbenchError> {
        let staged = self
            .stage_replacement(kernel.identity(), definition)
            .await?;
        self.admit_staged_successor(kernel, staged).await
    }

    async fn admit_staged_successor(
        &mut self,
        kernel: &KernelContext,
        mut staged: StagedHandler,
    ) -> Result<LocalActorRef, ResidentActorWorkbenchError> {
        let placement_custody = staged.placement_custody.clone();
        let root_admission = self
            .environment
            .root_admission_closed
            .clone()
            .read_owned()
            .await;
        if *root_admission {
            drop(staged);
            placement_custody
                .cleanup(
                    &self.environment.runner,
                    self.descriptor.placement().session,
                )
                .await?;
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "swarm admission is closed".into(),
            ));
        }
        let session_startup_custody = staged.session_startup.clone();
        let descriptor = staged
            .descriptor
            .clone()
            .with_supervisor_parent(kernel.supervisor_identity());
        let (transfer, custody) = tokio::sync::oneshot::channel();
        let session_startup = staged.session_startup.take();
        let exit_destination = staged.exit_destination.take();
        let mut behavior = Self::with_boot(
            descriptor,
            self.environment.clone(),
            ResidentBoot::Replacement(Box::new(PreparedSuccessor { staged, custody })),
            self.launch_worktrees.clone(),
        );
        behavior.child_session_startup = session_startup;
        behavior.child_placement_custody = Some(placement_custody.clone());
        behavior.exit_destination = exit_destination;
        let (successor, parent_admission) = match kernel.spawn_successor(behavior).await {
            Ok(successor) => successor,
            Err(error) => {
                if let Some(cleanup) = crate::local_actor::startup_cleanup(&error) {
                    placement_custody.record_startup_cleanup(cleanup.clone());
                    if !cleanup.is_confirmed() {
                        return Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                            "{error}; replacement candidate cleanup unconfirmed: {cleanup:?}"
                        )));
                    }
                } else {
                    placement_custody
                        .cleanup(
                            &self.environment.runner,
                            self.descriptor.placement().session,
                        )
                        .await?;
                }
                return Err(ResidentActorWorkbenchError::ActorProtocol(
                    error.to_string(),
                ));
            }
        };
        // Directory insertion committed this successor's membership. Admission
        // guards protect preparation, not the later custody transfer.
        drop(parent_admission);
        drop(root_admission);
        drop(session_startup_custody);
        if let Err(error) = self.transfer_worktree(successor.identity()).await {
            successor
                .abort_prepared_replacement()
                .await
                .map_err(|cleanup| {
                    ResidentActorWorkbenchError::ActorProtocol(format!(
                        "{error}; candidate cleanup failed: {cleanup}"
                    ))
                })?;
            return Err(error);
        }
        let fence = if let Some(sources) = &mut self.source_connections {
            sources.handoff(successor.clone(), LocalActorRef::fence_replacement)
        } else {
            kernel
                .resolve(kernel.identity())
                .ok_or_else(|| KernelBehaviorError {
                    detail: "replacement predecessor is absent from its directory".into(),
                    diagnostic: None,
                })
                .and_then(|actor| actor.fence_replacement())
        };
        if let Err(error) = fence {
            // No source moved and admission stayed open. Restore the original
            // owner with a new binding generation before discarding the successor.
            let restored = self.transfer_worktree(kernel.identity()).await;
            let cleanup = successor.abort_prepared_replacement().await;
            let mut detail = error.to_string();
            if let Err(restore) = restored {
                detail.push_str(&format!(
                    "; original workspace authority could not be restored: {restore}; custody retained"
                ));
            }
            if let Err(cleanup) = cleanup {
                detail.push_str(&format!("; candidate cleanup failed: {cleanup}"));
            }
            return Err(ResidentActorWorkbenchError::ActorProtocol(detail));
        }
        self.replacement_transfer = Some(ReplacementTransfer { send: transfer });
        Ok(successor)
    }

    async fn transfer_worktree(
        &mut self,
        owner: ActorRef,
    ) -> Result<(), ResidentActorWorkbenchError> {
        if let Some(custody) = self.worktree_custody.clone() {
            let transferred =
                tidepool_runtime::spawn_blocking_in_span(move || custody.transfer_to(owner))
                    .await
                    .map_err(|error| ResidentActorWorkbenchError::ActorProtocol(error.to_string()))?
                    .map_err(|error| {
                        ResidentActorWorkbenchError::ActorProtocol(error.to_string())
                    })?;
            self.worktree_custody = Some(transferred);
        }
        Ok(())
    }

    pub(super) fn transfer_replacement(
        &mut self,
        predecessor: &KernelContext,
        successor: &KernelContext,
    ) -> Result<(), ResidentActorWorkbenchError> {
        let successor_actor = successor.resolve(successor.identity()).ok_or_else(|| {
            ResidentActorWorkbenchError::ActorProtocol(
                "replacement successor is absent from its directory".into(),
            )
        })?;
        let transfer = self.replacement_transfer.take().ok_or_else(|| {
            ResidentActorWorkbenchError::ActorProtocol(
                "replacement transfer requested before a prepared replacement was admitted".into(),
            )
        })?;
        let retained = RetainedHandler {
            placement: self.descriptor.placement(),
            _standing: std::mem::replace(&mut self.standing, ResidentStanding::Terminal),
            _checkpoint: self.checkpoint.take(),
            _sources: std::mem::take(&mut self.sources),
            _shutdown_hook: self.shutdown_hook.take(),
        };
        self.retained_replacements.push(retained);
        let custody = ReplacementCustody {
            sources: self.source_connections.take(),
            worktree: self.worktree_custody.take(),
            retained: std::mem::take(&mut self.retained_replacements),
        };
        if let Err(custody) = transfer.send.send(custody) {
            self.source_connections = custody.sources;
            self.worktree_custody = custody.worktree;
            self.retained_replacements = custody.retained;
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "prepared successor lost its custody receiver; predecessor retains resources"
                    .into(),
            ));
        }
        self.environment
            .requests
            .transfer_owner(predecessor.identity(), &successor_actor);
        self.environment
            .commands
            .transfer_owner(predecessor.identity(), successor_actor.identity());
        for record in self.environment.actors.lock().values_mut() {
            if record.descriptor.supervisor_parent() == Some(predecessor.identity()) {
                record.descriptor = record
                    .descriptor
                    .clone()
                    .with_supervisor_parent(successor.identity());
            }
        }
        Ok(())
    }

    pub(super) fn activate_successor(
        &mut self,
    ) -> Result<KernelStep<()>, ResidentActorWorkbenchError> {
        let Some(ResidentBoot::Replacement(prepared)) = self.boot.take() else {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "actor has no staged successor".into(),
            ));
        };
        let PreparedSuccessor {
            staged,
            mut custody,
        } = *prepared;
        let custody = custody.try_recv().map_err(|error| {
            ResidentActorWorkbenchError::ActorProtocol(format!(
                "replacement custody was not transferred: {error}"
            ))
        })?;
        self.source_connections = custody.sources;
        self.worktree_custody = custody.worktree;
        self.retained_replacements = custody.retained;
        self.standing = ResidentStanding::Receiving(staged.receiver);
        self.checkpoint = Some(staged.checkpoint);
        self.shutdown_hook = staged.shutdown_hook;
        self.static_source_count = staged.sources.len();
        self.sources = staged.sources;
        self.sources.extend(staged.dynamic_sources);
        Ok(KernelStep::Continue(()))
    }

    async fn stage_replacement(
        &self,
        actor: ActorRef,
        definition: crate::ActorReplacementDefinition,
    ) -> Result<StagedHandler, ResidentActorWorkbenchError> {
        let mut candidate = ReplacementCandidate::new(
            definition.child.descriptor.placement(),
            self.descriptor.placement().session,
        );
        let placement_custody = candidate.placement.clone();
        let context = self.context(actor);
        let work = self.workbench_executions.lock().actor_scope_root(actor);
        let result = match work.retain_launch_placement(&context, placement_custody.clone()) {
            Ok(()) => {
                self.stage_replacement_inner(actor, definition, &mut candidate)
                    .await
            }
            Err(detail) => {
                drop(definition);
                Err(ResidentActorWorkbenchError::ActorProtocol(detail))
            }
        };
        // Drop provisional roots and the dedicated lease before reclamation.
        // A dropped future leaves the same placement with the retained actor owner.
        drop(candidate);
        if let Err(error) = &result {
            if let Err(cleanup) = placement_custody
                .cleanup(
                    &self.environment.runner,
                    self.descriptor.placement().session,
                )
                .await
            {
                return Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                    "{error}; replacement candidate cleanup failed: {cleanup}"
                )));
            }
        }
        result
    }

    async fn stage_replacement_inner(
        &self,
        actor: ActorRef,
        definition: crate::ActorReplacementDefinition,
        candidate: &mut ReplacementCandidate,
    ) -> Result<StagedHandler, ResidentActorWorkbenchError> {
        let reject = |detail: &str| ResidentActorWorkbenchError::ActorProtocol(detail.into());
        let checkpoint = match &self.standing {
            ResidentStanding::Paused(paused) => &paused.checkpoint,
            ResidentStanding::Receiving(_) => self
                .checkpoint
                .as_ref()
                .ok_or_else(|| reject("replacement requires committed state"))?,
            _ => {
                return Err(reject(
                    "replacement requires a paused or receiving stateful actor",
                ));
            }
        };
        let crate::start::CapturedChildLaunch {
            lifetime: _,
            mut descriptor,
            spawn: _,
            entry,
            launch_worktrees,
            record_workspace,
            seed,
            exit_destination,
        } = definition.child;
        if !launch_worktrees.is_empty() || record_workspace.is_some() {
            return Err(reject(
                "replacement must preserve the actor's worktree custody",
            ));
        }
        // Placement belongs to the successor; the entry still belongs to its
        // controlling caller and the checkpoint belongs to this predecessor.
        if descriptor.placement().session != self.descriptor.placement().session
            && !self.environment.runner.supports_child_sessions()
        {
            let placement = self
                .environment
                .runner
                .provision_fallback_scope(
                    descriptor.placement(),
                    self.descriptor.placement().session,
                    candidate.placement.clone(),
                )
                .await?;
            descriptor = descriptor
                .with_session(placement.session)
                .with_lexical_scope(placement.lexical_scope);
        } else if descriptor.placement().session != self.descriptor.placement().session {
            let provisioned = self
                .environment
                .runner
                .provision_child_session(
                    descriptor.placement().session,
                    descriptor.placement().resource_scope,
                    seed.as_ref(),
                    descriptor.source_layer(),
                )
                .await
                .map_err(ResidentActorWorkbenchError::ActorProtocol)?;
            descriptor = descriptor.with_lexical_scope(provisioned.lexical_scope);
            candidate.session_startup = Some(provisioned.startup_lease);
            candidate.placement.provisioned(descriptor.placement());
        }
        let context = descriptor.session_context(actor);
        let (entry, request_value) = self
            .environment
            .runner
            .prepare_replacement_inputs(
                context.clone(),
                entry,
                self.descriptor.placement().session,
                Arc::clone(&checkpoint.value),
            )
            .await?;
        let realm = context.placement.resource_scope;
        let runner = &self.environment.runner;
        let mut outcome = runner
            .run_rooted_application(context.clone(), entry, request_value, realm)
            .await?;
        let mut shutdown_hook = None;
        let mut sources = Vec::new();
        loop {
            match runner
                .capture_startup_step(context.clone(), outcome, realm)
                .await?
            {
                ResidentActorStartupStep::InstallSource {
                    continuation,
                    source,
                } => {
                    let original = self
                        .sources
                        .get(sources.len())
                        .filter(|_| sources.len() < self.static_source_count)
                        .ok_or_else(|| reject("replacement added a startup source"))?;
                    if original.target != source.target {
                        return Err(reject("replacement changed a startup source"));
                    }
                    sources.push(source);
                    outcome = runner.resume_unit(context.clone(), continuation).await?;
                }
                ResidentActorStartupStep::InstallShutdown(shutdown) => {
                    if shutdown_hook.is_some() {
                        return Err(reject("replacement installed two shutdown hooks"));
                    }
                    let (continuation, hook) = shutdown.into_parts();
                    shutdown_hook = Some(hook);
                    outcome = runner.resume_unit(context.clone(), continuation).await?;
                }
                ResidentActorStartupStep::Ready(readiness) => {
                    if sources.len() != self.static_source_count {
                        return Err(reject("replacement removed a startup source"));
                    }
                    outcome = runner.resume_readiness(context.clone(), readiness).await?;
                    break;
                }
                ResidentActorStartupStep::Attach(_) => {
                    return Err(reject(
                        "replacement staging cannot attach a native application",
                    ));
                }
            }
        }
        let ResidentActorBoundary::Checkpoint {
            continuation,
            site,
            value,
        } = runner
            .capture_replacement_boundary(context.clone(), outcome, realm)
            .await?
        else {
            return Err(reject(
                "replacement must checkpoint before executing effects",
            ));
        };
        let outcome = runner.resume_unit(context.clone(), continuation).await?;
        let ResidentActorBoundary::Receive(receiver) = runner
            .capture_replacement_boundary(context, outcome, realm)
            .await?
        else {
            return Err(reject(
                "replacement must install its receiver immediately after checkpoint",
            ));
        };
        if receiver.site != site {
            return Err(reject("replacement checkpoint and receiver sites differ"));
        }
        let dynamic_sources =
            if descriptor.placement().session == self.descriptor.placement().session {
                self.sources[self.static_source_count..].to_vec()
            } else {
                let mut imported = Vec::new();
                for source in &self.sources[self.static_source_count..] {
                    let entry = runner
                        .import_shared_custody(
                            Arc::clone(&source.entry),
                            self.descriptor.placement().session,
                            descriptor.placement().session,
                            descriptor.placement().resource_scope,
                        )
                        .await?;
                    imported.push(crate::request::sources::SourceBinding {
                        target: source.target,
                        entry: Arc::new(entry),
                    });
                }
                imported
            };
        Ok(StagedHandler {
            exit_destination,
            descriptor,
            receiver,
            checkpoint: StateCheckpoint {
                site,
                value: Arc::new(value),
            },
            shutdown_hook,
            sources,
            dynamic_sources,
            session_startup: candidate.session_startup.take(),
            placement_custody: candidate.placement.clone(),
            _placement_startup: Mutex::new(
                candidate
                    .startup
                    .take()
                    .expect("candidate owns startup admission"),
            ),
        })
    }
}
