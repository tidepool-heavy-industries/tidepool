use super::*;

/// Staging owns only the new code's roots. The running actor still owns its
/// state, queued inputs, source connections and supervised resources.
struct StagedHandler {
    descriptor: ActorDescriptor,
    receiver: InstalledReceiver,
    checkpoint: StateCheckpoint,
    shutdown_hook: Option<RootCustody>,
    sources: Vec<crate::request::sources::SourceBinding>,
}

pub(super) struct PreparedSuccessor {
    staged: StagedHandler,
    custody: tokio::sync::oneshot::Receiver<ReplacementCustody>,
}

/// Code and captured values can still reference a predecessor's realm. Keep
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
    worktree: Option<Arc<dyn crate::ForkWorkspaceCustody>>,
    retained: Vec<RetainedHandler>,
    _admissions: Vec<tokio::sync::OwnedRwLockReadGuard<bool>>,
}

pub(super) struct ReplacementTransfer {
    send: tokio::sync::oneshot::Sender<ReplacementCustody>,
    admissions: Vec<tokio::sync::OwnedRwLockReadGuard<bool>>,
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
        let placement = staged.descriptor.placement();
        let root_admission = self
            .environment
            .root_admission_closed
            .clone()
            .read_owned()
            .await;
        if *root_admission {
            drop(staged);
            self.environment
                .runner
                .retire_root_placement(placement)
                .await?;
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "swarm admission is closed".into(),
            ));
        }
        let descriptor = staged
            .descriptor
            .clone()
            .with_supervisor_parent(kernel.supervisor_identity());
        let (transfer, custody) = tokio::sync::oneshot::channel();
        let behavior = Self::with_boot(
            descriptor,
            self.environment.clone(),
            ResidentBoot::Replacement(Box::new(PreparedSuccessor { staged, custody })),
            self.launch_worktrees.clone(),
        );
        let (successor, parent_admission) = match kernel.spawn_successor(behavior).await {
            Ok(successor) => successor,
            Err(error) => {
                self.environment
                    .runner
                    .retire_root_placement(placement)
                    .await?;
                return Err(ResidentActorWorkbenchError::ActorProtocol(
                    error.to_string(),
                ));
            }
        };
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
        self.replacement_transfer = Some(ReplacementTransfer {
            send: transfer,
            admissions: std::iter::once(root_admission)
                .chain(parent_admission)
                .collect(),
        });
        Ok(successor)
    }

    async fn transfer_worktree(
        &mut self,
        owner: ActorRef,
    ) -> Result<(), ResidentActorWorkbenchError> {
        if let Some(custody) = self.worktree_custody.clone() {
            let transferred = tokio::task::spawn_blocking(move || custody.transfer_to(owner))
                .await
                .map_err(|error| ResidentActorWorkbenchError::ActorProtocol(error.to_string()))?
                .map_err(|error| ResidentActorWorkbenchError::ActorProtocol(error.to_string()))?;
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
        let transfer = self
            .replacement_transfer
            .take()
            .expect("prepared replacement owns its transfer");
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
            _admissions: transfer.admissions,
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
        self.sources = staged.sources;
        Ok(KernelStep::Continue(()))
    }

    async fn stage_replacement(
        &self,
        actor: ActorRef,
        definition: crate::ActorReplacementDefinition,
    ) -> Result<StagedHandler, ResidentActorWorkbenchError> {
        let placement = definition.child.descriptor.placement();
        let result = self.stage_replacement_inner(actor, definition).await;
        if let Err(error) = &result {
            // Temporary roots drop before retiring this isolated scope.
            if let Err(cleanup) = self
                .environment
                .runner
                .retire_root_placement(placement)
                .await
            {
                return Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                    "{error}; replacement candidate cleanup failed at {placement:?}: {cleanup}"
                )));
            }
        }
        result
    }

    async fn stage_replacement_inner(
        &self,
        actor: ActorRef,
        definition: crate::ActorReplacementDefinition,
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
                ))
            }
        };
        let crate::start::CapturedChildLaunch {
            descriptor,
            entry,
            launch_worktrees,
            fork_workspace,
        } = definition.child;
        if descriptor.placement().session != self.descriptor.placement().session {
            return Err(reject("replacement crossed a resident machine boundary"));
        }
        if launch_worktrees != self.launch_worktrees || fork_workspace.is_some() {
            return Err(reject(
                "replacement must preserve the actor's worktree custody",
            ));
        }
        let context = descriptor.session_context(actor);
        let realm = context.placement.resource_scope;
        let runner = &self.environment.runner;
        let mut outcome = runner
            .run_rooted_application(context.clone(), entry, checkpoint.value.clone(), realm)
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
                    let original = self.sources.get(sources.len()).ok_or_else(|| {
                        reject("replacement added a source; create a new actor to change sources")
                    })?;
                    if original.target != source.target {
                        return Err(reject("replacement changed its fixed source graph"));
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
                    if sources.len() != self.sources.len() {
                        return Err(reject(
                            "replacement removed a source; create a new actor to change sources",
                        ));
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
        Ok(StagedHandler {
            descriptor,
            receiver,
            checkpoint: StateCheckpoint {
                site,
                value: Arc::new(value),
            },
            shutdown_hook,
            sources,
        })
    }
}
