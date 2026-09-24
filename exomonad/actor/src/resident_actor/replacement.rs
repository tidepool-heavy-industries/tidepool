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
            // If this replacement minted itself a fresh session
            // (`child_session_eligibility`) and provisioning got as far as
            // publishing it into the shared registry before something else
            // failed, it is now orphaned — nothing will ever admit onto it.
            // Discarding a session that was never provisioned in the first
            // place (ineligible, or the host fell back to the predecessor's
            // own session) is always safe: it is simply not a member of
            // `child_sessions`.
            if placement.session != self.descriptor.placement().session {
                self.environment
                    .runner
                    .discard_child_session(placement.session);
            }
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
            mut descriptor,
            entry,
            launch_worktrees,
            fork_workspace,
        } = definition.child;
        if launch_worktrees != self.launch_worktrees || fork_workspace.is_some() {
            return Err(reject(
                "replacement must preserve the actor's worktree custody",
            ));
        }
        // See `resident_actor.rs::try_start_child`'s matching resolution: a
        // replacement that names the predecessor's own session needs
        // nothing further. One that names a freshly minted session instead
        // (an eligible `SelectedContext` replacement) provisions that
        // session's dedicated machine and crosses the entry into it with
        // `ResidentActorRunner::transfer_custody`, getting the new
        // session's own lexical scope, never the predecessor's.
        let (entry, request_value) =
            if descriptor.placement().session == self.descriptor.placement().session {
                (entry, checkpoint.value.clone())
            } else if !self.environment.runner.supports_child_sessions() {
                // See `resident_actor.rs::try_start_child`'s matching fallback:
                // `capture_decoded` minted no real lexical scope for this
                // (eligible) replacement, only a placeholder.
                let lexical_scope = self
                    .environment
                    .runner
                    .mint_lexical_scope(self.descriptor.placement().session)
                    .await?;
                descriptor = descriptor
                    .with_session(self.descriptor.placement().session)
                    .with_lexical_scope(lexical_scope);
                (entry, checkpoint.value.clone())
            } else {
                let child_session = descriptor.placement().session;
                let lexical_scope = self
                    .environment
                    .runner
                    .provision_child_session(child_session, descriptor.placement().resource_scope)
                    .await
                    .map_err(ResidentActorWorkbenchError::ActorProtocol)?;
                descriptor = descriptor.with_lexical_scope(lexical_scope);
                let entry = self
                    .environment
                    .runner
                    .transfer_custody(
                        entry,
                        self.descriptor.placement().session,
                        child_session,
                        descriptor.placement().resource_scope,
                    )
                    .await?;
                // `checkpoint.value` is a shared, non-consuming root: several
                // callers may still read the predecessor's own checkpoint (a
                // retry, or recovery after this replacement itself fails), so
                // it crosses through the SAME borrow-export path
                // `resume_progress_observation` uses for a published progress
                // snapshot (`import_shared_custody`), never a consuming
                // `transfer_custody` — the predecessor's own copy stays exactly
                // as it was.
                let request_value = self
                    .environment
                    .runner
                    .import_shared_custody(
                        Arc::clone(&checkpoint.value),
                        self.descriptor.placement().session,
                        child_session,
                        descriptor.placement().resource_scope,
                    )
                    .await?;
                (entry, Arc::new(request_value))
            };
        let context = descriptor.session_context(actor);
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
