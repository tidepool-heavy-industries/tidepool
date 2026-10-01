//! Single-admission preparation and watch tasks retain one execution cursor.

use super::*;
use crate::{OwnedWorkbenchCompletion, OwnedWorkbenchTask, WorkbenchAdvance, WorkbenchDispatch};

/// Source and tool owners admitted once for this hosted execution. Compiler
/// recipe observations are added by the workbench's original snapshot owner.
pub(crate) struct WorkbenchCompilationAuthority {
    source: crate::CheckpointSourceLayer,
    // Keep the admitted implementation alive until this execution releases it.
    _installed_tools: Option<crate::InstalledToolLease>,
    authority_digest: [u8; 32],
}

impl WorkbenchCompilationAuthority {
    pub(super) fn admit(
        context: ActorSessionContext,
        source: crate::CheckpointSourceLayer,
        installed_tools: Option<crate::InstalledToolLease>,
        source_layers: Option<&crate::ActorSourceLayerResolver>,
    ) -> Result<(ActorSessionContext, Arc<Self>), KernelInvocationFailure> {
        let actor = context.actor;
        let reject = |detail| KernelInvocationFailure::Rejected { actor, detail };
        if let Some(layers) = source_layers {
            layers
                .validate_source_authority(&source)
                .map_err(|error| reject(format!("cannot admit source authority: {error}")))?;
        } else if source.is_owned() {
            return Err(reject(
                "no configured owner validates issued source authority".into(),
            ));
        }
        let context = context
            .with_issued_source(&source)
            .map_err(|error| reject(format!("cannot select retained source authority: {error}")))?;
        if let Some(lease) = &installed_tools {
            if lease.actor() != actor || lease.source() != &source {
                return Err(reject(
                    "issued tool installation does not match admitted actor and source".into(),
                ));
            }
        }
        let mut digest = blake3::Hasher::new();
        let mut frame = |bytes: &[u8]| {
            digest.update(&(bytes.len() as u64).to_le_bytes());
            digest.update(bytes);
        };
        frame(b"exomonad-workbench-compilation-authority-v1");
        frame(&actor.id.0.to_le_bytes());
        frame(&actor.incarnation.0.to_le_bytes());
        frame(&source.semantic_digest());
        match &installed_tools {
            Some(lease) => {
                frame(b"issued-tool-lease");
                frame(&lease.source().semantic_digest());
                match lease.tools() {
                    Some(tools) => {
                        frame(b"installed-handler");
                        frame(&tools.install.to_le_bytes());
                        match &tools.revision {
                            Some(revision) => {
                                frame(b"revision");
                                frame(revision.as_bytes());
                            }
                            None => frame(b"no-revision"),
                        }
                    }
                    None => frame(b"no-handler"),
                }
            }
            None => frame(b"no-tool-lease"),
        }
        Ok((
            context,
            Arc::new(Self {
                source,
                _installed_tools: installed_tools,
                authority_digest: *digest.finalize().as_bytes(),
            }),
        ))
    }

    pub(crate) fn source(&self) -> &crate::CheckpointSourceLayer {
        &self.source
    }

    #[cfg(test)]
    fn installed_tools(&self) -> Option<&crate::InstalledToolLease> {
        self._installed_tools.as_ref()
    }

    pub(crate) fn authority_digest(&self) -> [u8; 32] {
        self.authority_digest
    }
}

/// Issued only from the actor owner's actual descriptor and selected plane.
/// It is retained with the original private execution, never rebuilt at resume.
pub(crate) struct WorkbenchPublicOwner {
    actor: ActorRef,
    placement: crate::ActorPlacement,
    durable: Option<tidepool_runtime::session::RecoveryPublicOwner>,
}

/// Plain resource owners retained by the existing native continuation entry.
/// No abort callback, session registry, or input-retirement callback belongs here.
pub(crate) struct ExecutionResourceOwners {
    _compilation: Option<Arc<WorkbenchCompilationAuthority>>,
    public: Arc<WorkbenchPublicOwner>,
    private: std::sync::OnceLock<Arc<tidepool_runtime::session::PrivateExecutionAdmission>>,
}

impl ExecutionResourceOwners {
    fn new(
        compilation: Arc<WorkbenchCompilationAuthority>,
        public: Arc<WorkbenchPublicOwner>,
    ) -> Arc<Self> {
        Arc::new(Self {
            _compilation: Some(compilation),
            public,
            private: std::sync::OnceLock::new(),
        })
    }

    fn for_inspection(public: Arc<WorkbenchPublicOwner>) -> Arc<Self> {
        Arc::new(Self {
            _compilation: None,
            public,
            private: std::sync::OnceLock::new(),
        })
    }

    pub(crate) fn authorizes_cleanup_context(&self, context: &ActorSessionContext) -> bool {
        let original = self.public.placement;
        context.actor == self.public.actor
            && context.placement.session == original.session
            && context.placement.resource_scope == original.resource_scope
            && (context.placement.lexical_scope == original.lexical_scope
                || self.private.get().is_some_and(|private| {
                    private.view().session() == context.placement.session
                        && private.private_scope() == context.placement.lexical_scope
                }))
    }

    pub(super) fn retain_private(
        &self,
        admission: Arc<tidepool_runtime::session::PrivateExecutionAdmission>,
    ) -> Result<(), ResidentActorWorkbenchError> {
        if self._compilation.is_none() {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "inspection resources cannot admit a private authored execution".into(),
            ));
        }
        if admission.view().session() != self.public.placement.session
            || admission.admitted_public().scope != self.public.placement.lexical_scope
        {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "private resources differ from the original publication owner".into(),
            ));
        }
        if let Err(admission) = self.private.set(admission) {
            if !Arc::ptr_eq(
                self.private
                    .get()
                    .expect("original private owner already retained"),
                &admission,
            ) {
                return Err(ResidentActorWorkbenchError::ActorProtocol(
                    "execution cannot replace its original private resource owner".into(),
                ));
            }
        }
        Ok(())
    }
}

impl WorkbenchPublicOwner {
    pub(super) fn issue(
        context: &ActorSessionContext,
        descriptor: &ActorDescriptor,
        durable: Option<tidepool_runtime::session::RecoveryPublicOwner>,
    ) -> Result<Arc<Self>, KernelInvocationFailure> {
        let refuse = |detail: &str| KernelInvocationFailure::Rejected {
            actor: context.actor,
            detail: detail.into(),
        };
        if context.placement != descriptor.placement() {
            return Err(refuse(
                "publication owner differs from the admitted actor placement",
            ));
        }
        match (descriptor.persistence_policy(), &durable) {
            (crate::ActorPersistencePolicy::Ephemeral, None) => {}
            (crate::ActorPersistencePolicy::Durable, Some(owner)) => {
                let expected = descriptor.actor_path().and_then(|path| {
                    tidepool_runtime::session::RecoveryPublicOwner::new(
                        path,
                        context.actor.incarnation.0,
                    )
                });
                if expected.as_ref() != Some(owner) {
                    return Err(refuse(
                        "durable publication owner differs from canonical actor identity",
                    ));
                }
            }
            _ => {
                return Err(refuse(
                    "publication plane differs from requested persistence",
                ));
            }
        }
        Ok(Arc::new(Self {
            actor: context.actor,
            placement: context.placement,
            durable,
        }))
    }

    pub(crate) fn durable(&self) -> Option<&tidepool_runtime::session::RecoveryPublicOwner> {
        self.durable.as_ref()
    }

    pub(crate) fn matches_context(&self, context: &ActorSessionContext) -> bool {
        self.actor == context.actor && self.placement == context.placement
    }

    pub(super) fn actor(&self) -> ActorRef {
        self.actor
    }
    pub(super) fn placement(&self) -> crate::ActorPlacement {
        self.placement
    }
}

struct OwnedExecution<H, O> {
    state: WorkbenchExecutionState,
    workbench: Option<crate::ResidentActorWorkbench<H, O>>,
    private: Option<Arc<crate::resident_workbench::ExecutionPrivateScope>>,
    timing: Option<crate::call_timing::CallScope>,
    cleanup: crate::resident_workbench::ParkedHoleAbortGuard,
    resources: Arc<ExecutionResourceOwners>,
    observation: crate::ActorRuntimeObservationHandle,
    retirement: crate::RetainedActorExit,
}

impl<H, O> Drop for OwnedExecution<H, O> {
    fn drop(&mut self) {
        // Lost actor tasks cannot admit further work. The original journal and
        // resource owners retain any cleanup that could not be observed.
        self.state.effects.invocation_work.close();
    }
}

pub(super) struct WorkbenchUnitStart {
    pub request: WorkbenchUnitStartRequest,
    pub span: tracing::Span,
}

pub(super) enum WorkbenchUnitStartRequest {
    Tool {
        dispatch: Arc<RootCustody>,
        name: String,
        arguments: serde_json::Value,
    },
    Prepared {
        block: ParsedBlock,
        item: crate::resident_workbench::PreparedCellItem,
        display_remaining: usize,
    },
}

pub(super) async fn begin_unit<H, O>(
    workbench: &crate::ResidentActorWorkbench<H, O>,
    context: ActorSessionContext,
    start: WorkbenchUnitStart,
) -> Result<ResidentWorkbenchStep, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    async {
        match start.request {
            WorkbenchUnitStartRequest::Tool {
                dispatch,
                name,
                arguments,
            } => {
                workbench
                    .begin_tool(context, dispatch, name, arguments)
                    .await
            }
            WorkbenchUnitStartRequest::Prepared {
                block,
                item,
                display_remaining,
            } => {
                workbench
                    .begin_prepared_cell_item(context, block, item, display_remaining)
                    .await
            }
        }
    }
    .instrument(start.span)
    .await
}

pub(super) enum WorkbenchFragmentRequest {
    Settle {
        fragment: ResidentWorkbenchFragment,
        outcome: ResidentOutcome,
    },
    ReplyRejection {
        continuation: ResidentHole,
        error: crate::ReplyError,
    },
}

pub(super) enum WorkbenchFragmentAdvance {
    Captured {
        fragment: ResidentWorkbenchFragment,
        boundary: ResidentActorBoundary,
    },
    Settled(ResidentWorkbenchStep),
    Resumed(ResidentOutcome),
}

/// Native settlement and boundary decoding wait only on the original checkout.
/// The actor applies the returned fragment or boundary under the same step fence.
pub(super) async fn advance_fragment<H, O>(
    workbench: &crate::ResidentActorWorkbench<H, O>,
    runner: &ResidentActorRunner<H, O>,
    context: ActorSessionContext,
    request: WorkbenchFragmentRequest,
) -> Result<WorkbenchFragmentAdvance, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    let step = match request {
        WorkbenchFragmentRequest::Settle { fragment, outcome } => {
            workbench
                .settle_item(context.clone(), fragment, outcome)
                .await?
        }
        WorkbenchFragmentRequest::ReplyRejection {
            continuation,
            error,
        } => {
            return runner
                .resume_reply_rejection(context, continuation, error)
                .await
                .map(WorkbenchFragmentAdvance::Resumed);
        }
    };
    match step {
        ResidentWorkbenchStep::Running { fragment, outcome } => {
            let boundary = runner
                .capture_boundary(context.clone(), *outcome, context.placement.resource_scope)
                .await?;
            Ok(WorkbenchFragmentAdvance::Captured {
                fragment: *fragment,
                boundary,
            })
        }
        step => Ok(WorkbenchFragmentAdvance::Settled(step)),
    }
}

impl<H, O> OwnedExecution<H, O> {
    fn expire_after_tool(&mut self) {
        if let Some(stamp) = self
            .state
            .cursor
            .running
            .as_mut()
            .and_then(|current| current.inflight_effect.take())
        {
            record_workbench_operation(
                &mut self.state.cursor.unit.operations,
                self.state.request.execution_id(),
                self.state.cursor.index,
                stamp.ordinal,
                &stamp.effect,
                stamp.started.elapsed(),
                WorkbenchOperationDisposition::Unknown,
            );
        }
        let slot = self
            .state
            .cursor
            .after_tool
            .as_mut()
            .expect("timeout retains the original after-tool frame");
        slot.answer = Some(WorkbenchAfterToolAnswer::TimedOut);
        slot.enforce_deadline = false;
    }

    fn scopes(
        &self,
    ) -> (
        crate::call_timing::CallTimingRegistration,
        crate::resident_workbench::ParkedHoleAbortRegistration,
    ) {
        (
            self.timing
                .as_ref()
                .expect("one execution timing owner")
                .registration(),
            self.cleanup.registration(),
        )
    }
}

impl<H, O> ResidentKernelBehavior<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    /// A typed task owns only this admitted execution. The scheduler fences its
    /// result before this synchronous application can touch actor-owned state.
    fn owned_step_task<T, Run, Apply>(
        mut owned: OwnedExecution<H, O>,
        run: Run,
        apply: Apply,
    ) -> OwnedWorkbenchTask<Self>
    where
        T: Send + 'static,
        Run: for<'a> FnOnce(&'a mut OwnedExecution<H, O>) -> futures_util::future::BoxFuture<'a, T>
            + Send
            + 'static,
        Apply: FnOnce(
                &mut Self,
                &KernelContext,
                OwnedExecution<H, O>,
                T,
            ) -> Result<WorkbenchAdvance<Self>, KernelInvocationFailure>
            + Send
            + 'static,
    {
        OwnedWorkbenchTask::new(Box::pin(async move {
            let (timing, cleanup) = owned.scopes();
            let deadline = owned
                .state
                .cursor
                .after_tool
                .as_ref()
                .filter(|slot| slot.enforce_deadline)
                .map(|slot| (slot.frame.deadline(), slot.frame.entered()));
            let span = owned
                .state
                .cursor
                .after_tool
                .as_ref()
                .map(|slot| slot.span.clone())
                .unwrap_or_else(tracing::Span::none);
            let observation = owned.observation.clone();
            let index = owned.state.cursor.index;
            let total = owned.state.request.items.len();
            let retirement = owned.retirement.clone();
            let control = owned
                .state
                .effects
                .control
                .as_ref()
                .expect("execution retains its cancellation owner")
                .clone();
            let cancel = owned
                .state
                .effects
                .control
                .as_ref()
                .expect("execution retains its cancellation owner")
                .native_cancel();
            let completed = {
                let operation = async {
                    match deadline {
                        Some((deadline, entered)) => {
                            let operation = run(&mut owned);
                            tokio::pin!(operation);
                            let mut progress = tokio::time::interval_at(
                                entered + crate::after_tool::AFTER_TOOL_PROGRESS,
                                crate::after_tool::AFTER_TOOL_PROGRESS,
                            );
                            loop {
                                tokio::select! {
                                    biased;
                                    completed = &mut operation => break Some(completed),
                                    () = tokio::time::sleep_until(deadline) => {
                                        control.request_cancellation();
                                        operation.await;
                                        break None;
                                    },
                                    _ = progress.tick() => observation.publish_workbench_posture(
                                        crate::ActorWorkbenchPosture::AwaitingEffect {
                                            input_unit_index: index, total,
                                            effect: format!("after-tool slot, {}s elapsed", entered.elapsed().as_secs()),
                                        },
                                    ),
                                }
                            }
                        },
                        None => Some(run(&mut owned).await),
                    }
                }.instrument(span.clone());
                let running = crate::resident_workbench::with_invocation_cancellation(
                    cancel,
                    timing.scope(cleanup.scope(operation)),
                );
                tokio::pin!(running);
                tokio::select! {
                    completed = &mut running => completed,
                    _ = retirement.wait_requested_shutdown() => {
                        control.request_cancellation();
                        running.await
                    }
                }
            };
            OwnedWorkbenchCompletion::advance(move |behavior: &mut Self, kernel| {
                let (timing, cleanup) = owned.scopes();
                timing.sync_scope(|| {
                    cleanup.sync_scope(|| match completed {
                        Some(completed) => apply(behavior, kernel, owned, completed),
                        None => {
                            owned.expire_after_tool();
                            Ok(WorkbenchAdvance::Park(
                                behavior.finish_owned_after_tool_task(owned, kernel.clone()),
                            ))
                        }
                    })
                })
            })
        }))
    }

    pub(super) fn dispatch_owned_workbench(
        &mut self,
        kernel: &KernelContext,
        invocation: crate::ActorWorkbenchInvocation,
        control: Option<Arc<crate::WorkbenchExecutionControl>>,
    ) -> WorkbenchDispatch<Self> {
        // Repair paths retain their serial admission until their source-publication task is converted.
        if invocation.request.tool_call().is_some_and(|call| {
            matches!(
                call.name.as_str(),
                crate::reload_spec_tool::RELOAD_SPEC_TOOL
                    | crate::reload_helpers_tool::RELOAD_HELPERS_TOOL
            )
        }) {
            return WorkbenchDispatch::Sequential {
                invocation,
                control,
            };
        }
        let admitted = match self.preflight_workbench(kernel.identity(), invocation, control) {
            Ok(WorkbenchPreflight::Admitted(admitted)) => admitted,
            Ok(WorkbenchPreflight::Retained(reply)) => {
                return terminal_task(reply.map(KernelStep::Continue));
            }
            Err(error) => return terminal_task(Err(error)),
        };
        let WorkbenchAdmission {
            context,
            request,
            compilation_authority,
            public_owner,
            installed_tools,
            admitted_source,
            capture,
            control,
            invocation,
            ..
        } = admitted;
        let inspection = request
            .tool_call()
            .filter(|call| call.name == crate::status_tool::STATUS_TOOL)
            .map(|call| crate::status_tool::parse(call.arguments.clone()));
        if inspection.is_none() && compilation_authority.is_none() {
            return terminal_task(Err(KernelInvocationFailure::Rejected {
                actor: context.actor,
                detail: "authored execution has no compilation authority".into(),
            }));
        }
        let Some(workbench) = self.active_workbench() else {
            return terminal_task(Err(KernelInvocationFailure::Rejected {
                actor: context.actor,
                detail: "actor application has no active Haskell workbench".into(),
            }));
        };
        let workbench = match &compilation_authority {
            Some(authority) => workbench.with_compilation_authority(authority.clone()),
            None => workbench,
        };
        let workbench = workbench.with_json_input(
            request
                .input
                .as_ref()
                .map(tidepool_runtime::session::normalize_workbench_input),
        );
        let execution = request.execution_id().cloned();
        if let Some(execution) = &execution {
            self.workbench_executions
                .lock()
                .begin(execution, request.clone(), invocation.as_ref());
        }
        let replay_request = execution.as_ref().map(|_| request.clone());
        let reservation_owner = RequestReservationOwner::Workbench {
            execution: execution.unwrap_or_else(|| {
                WorkbenchExecutionId::from_digest(*uuid::Uuid::new_v4().as_bytes())
            }),
            attempt: crate::request::WorkbenchReservationAttempt::fresh(),
        };
        let invocation_work = InvocationWork::new(context.actor, reservation_owner.clone());
        self.workbench_executions.lock().retain_invocation_work(
            invocation_work.clone(),
            request.execution_id(),
            invocation.as_ref(),
        );
        let kind = request
            .tool_call()
            .map(|call| call.name.clone())
            .unwrap_or_else(|| "cell".into());
        let (actor, incarnation) = actor_address(context.actor);
        let timing = crate::call_timing::CallScope::new(kind, actor as u64, incarnation as u64);
        let resources = match compilation_authority {
            Some(authority) => ExecutionResourceOwners::new(authority, public_owner),
            None => ExecutionResourceOwners::for_inspection(public_owner),
        };
        let control = Some(control.unwrap_or_else(crate::WorkbenchExecutionControl::untracked));
        let cleanup = workbench.continuation_cleanup_owner(
            context.clone(),
            "hosted execution abandoned before exact continuation settlement".into(),
            resources.clone(),
        );
        let owned = OwnedExecution {
            observation: self.runtime_observation.clone(),
            retirement: kernel.retained_exit(),
            state: WorkbenchExecutionState {
                effects: WorkbenchEffectState {
                    park_effects: true,
                    context: context.clone(),
                    public_visibility: None,
                    control,
                    installed_tools,
                    admitted_source,
                    reservation_owner,
                    invocation_work,
                    publication: ForkPublication::Workbench {
                        boundary: request.fork_boundary().cloned(),
                        capture,
                    },
                    after_tool_active: false,
                },
                request,
                replay_request,
                invocation,
                cursor: WorkbenchCursor::default(),
            },
            workbench: Some(workbench),
            private: None,
            timing: Some(timing),
            cleanup,
            resources,
        };
        if let Some(inspection) = inspection {
            return WorkbenchDispatch::Owned(match inspection {
                Ok(view) => self.owned_inspection_task(kernel, owned, view),
                Err(error) => Self::finish_owned_task(
                    owned,
                    Err(workbench_failure(
                        &[],
                        0,
                        1,
                        ResidentActorWorkbenchError::ActorProtocol(error.to_string()),
                    )),
                ),
            });
        }
        let runner = self.environment.runner.clone();
        WorkbenchDispatch::Owned(Self::owned_step_task(
            owned,
            move |owned| {
                Box::pin(async move {
                    let decision = owned
                        .state
                        .effects
                        .control
                        .as_ref()
                        .expect("owned execution retains its original control")
                        .publication_decision();
                    tracing::info!(target: "exomonad_actor::workbench_phase", actor = %context.actor, phase = "private_begin_started", "workbench phase");
                    let private = Arc::new(
                        runner
                            .begin_private_execution(
                                context,
                                owned.resources.public.clone(),
                                decision,
                            )
                            .await?,
                    );
                    tracing::info!(target: "exomonad_actor::workbench_phase", actor = %owned.state.effects.context.actor, phase = "private_begin_completed", "workbench phase");
                    owned.resources.retain_private(private.admission.clone())?;
                    owned.state.effects.public_visibility = Some(private.admitted_public.clone());
                    owned.state.effects.context.placement.lexical_scope = private.private_scope;
                    let workbench = owned
                        .workbench
                        .take()
                        .expect("original workbench is installed once");
                    owned.workbench = Some(workbench.with_private_execution(private.clone()));
                    owned.private = Some(private);
                    tracing::info!(target: "exomonad_actor::workbench_phase", actor = %owned.state.effects.context.actor, phase = "cell_prepare_started", "workbench phase");
                    let cell = match owned.state.request.cell_source() {
                        Some(source) => Some(
                            owned
                                .workbench
                                .as_ref()
                                .expect("original workbench retains its private owner")
                                .prepare_cell(
                                    owned.state.effects.context.clone(),
                                    source.to_owned(),
                                )
                                .await,
                        ),
                        None => None,
                    };
                    tracing::info!(target: "exomonad_actor::workbench_phase", actor = %owned.state.effects.context.actor, phase = "cell_prepare_completed", "workbench phase");
                    Ok::<_, ResidentActorWorkbenchError>(cell)
                })
            },
            |behavior, _kernel, mut owned, prepared| {
                let cell = match prepared {
                    Ok(cell) => cell,
                    Err(error) => {
                        return Ok(WorkbenchAdvance::Park(Self::finish_owned_task(
                            owned,
                            Err(workbench_failure(&[], 0, 1, error)),
                        )));
                    }
                };
                if let Some(cell) = cell {
                    match install_cell_preparation(
                        &mut owned.state.request,
                        &mut owned.state.cursor,
                        cell,
                    ) {
                        Ok(Some(step)) => {
                            return Ok(WorkbenchAdvance::Park(Self::finish_owned_task(
                                owned,
                                Ok(step),
                            )));
                        }
                        Err(error) => {
                            return Ok(WorkbenchAdvance::Park(Self::finish_owned_task(
                                owned,
                                Err(error),
                            )));
                        }
                        Ok(None) => {}
                    }
                } else {
                    owned.state.cursor.preparation_done = true;
                }
                behavior.runtime_observation.publish_workbench_posture(
                    crate::ActorWorkbenchPosture::RunningUnit {
                        input_unit_index: 0,
                        total: owned.state.request.items.len(),
                    },
                );
                Ok(WorkbenchAdvance::Park(Self::continue_owned_task(owned)))
            },
        ))
    }

    fn owned_inspection_task(
        &self,
        kernel: &KernelContext,
        owned: OwnedExecution<H, O>,
        view: crate::status_tool::StatusView,
    ) -> OwnedWorkbenchTask<Self> {
        use crate::status_tool::StatusView as View;
        use inspection_wait::{InspectionRequest, InspectionResult};
        let actor = owned.state.effects.context.actor;
        enum Selection {
            Native(InspectionRequest),
            Rendered(String),
        }
        let selection = match view {
            View::Recovery => Selection::Native(InspectionRequest::Recovery),
            View::Bindings => Selection::Native(InspectionRequest::Bindings),
            View::Live => Selection::Native(InspectionRequest::Live),
            View::Changed => {
                Selection::Rendered(self.status_text(kernel, actor, StatusView::Concise, true))
            }
            View::Summary => {
                Selection::Rendered(self.status_text(kernel, actor, StatusView::Concise, false))
            }
            View::Revisions => Selection::Rendered(self.revisions_status_text(kernel, actor)),
            View::Detailed => {
                Selection::Rendered(self.status_text(kernel, actor, StatusView::Expanded, false))
            }
            View::Lineage => {
                Selection::Rendered(self.status_text(kernel, actor, StatusView::Lineage, false))
            }
            View::Trace => {
                Selection::Rendered(self.status_text(kernel, actor, StatusView::Trace, false))
            }
            View::Watches => {
                Selection::Rendered(self.status_text(kernel, actor, StatusView::Watches, false))
            }
        };
        Self::owned_step_task(
            owned,
            move |owned| {
                Box::pin(async move {
                    match selection {
                        Selection::Native(request) => {
                            inspection_wait::inspect(
                                owned
                                    .workbench
                                    .as_ref()
                                    .expect("inspection retains original workbench"),
                                owned.state.effects.context.clone(),
                                request,
                            )
                            .await
                        }
                        Selection::Rendered(output) => Ok(InspectionResult::Rendered(output)),
                    }
                })
            },
            |behavior, _kernel, owned, inspected| {
                let result = inspected
                    .map(|inspected| {
                        let output = match inspected {
                            InspectionResult::Rendered(output) => output,
                            InspectionResult::Live(bindings) => behavior
                                .live_status_text(owned.state.effects.context.actor, &bindings),
                        };
                        KernelStep::Continue(workbench_response(
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
                        ))
                    })
                    .map_err(|error| workbench_failure(&[], 0, 1, error));
                Ok(WorkbenchAdvance::Park(Self::finish_owned_task(
                    owned, result,
                )))
            },
        )
    }

    /// The existing serial driver keeps all unconverted handlers and terminal
    /// cleanup. It resumes this same cursor and yields its captured effect wait.
    fn continue_owned_task(mut owned: OwnedExecution<H, O>) -> OwnedWorkbenchTask<Self> {
        OwnedWorkbenchTask::new(Box::pin(async move {
            OwnedWorkbenchCompletion::advance_async(move |behavior: &mut Self, kernel| {
                Box::pin(async move {
                    let (timing, cleanup) = owned.scopes();
                    let deadline = owned
                        .state
                        .cursor
                        .after_tool
                        .as_ref()
                        .filter(|slot| slot.enforce_deadline)
                        .map(|slot| slot.frame.deadline());
                    let result = {
                        let cancel = owned
                            .state
                            .effects
                            .control
                            .as_ref()
                            .expect("execution retains its cancellation owner")
                            .native_cancel();
                        let advance = crate::resident_workbench::with_invocation_cancellation(
                            cancel,
                            timing.scope(cleanup.scope(behavior.execute_workbench(
                                kernel,
                                &mut owned.state,
                                owned.workbench.as_ref(),
                            ))),
                        );
                        tokio::pin!(advance);
                        match deadline {
                            Some(deadline) => tokio::select! {
                                biased;
                                result = &mut advance => Some(result),
                                () = tokio::time::sleep_until(deadline) => None,
                            },
                            None => Some(advance.await),
                        }
                    };
                    let Some(result) = result else {
                        owned.expire_after_tool();
                        return Ok(WorkbenchAdvance::Park(
                            behavior.finish_owned_after_tool_task(owned, kernel.clone()),
                        ));
                    };
                    let task = match result {
                        Ok(WorkbenchRunAdvance::ParkEffect) => {
                            behavior.owned_effect_task(owned, kernel.clone())
                        }
                        Ok(WorkbenchRunAdvance::ParkUnit) => Self::owned_unit_task(owned),
                        Ok(WorkbenchRunAdvance::ParkNative) => behavior.owned_fragment_task(owned),
                        Ok(WorkbenchRunAdvance::ParkAfterToolStart) => {
                            Self::begin_owned_after_tool_task(owned)
                        }
                        Ok(WorkbenchRunAdvance::ParkAfterToolFinish) => {
                            behavior.finish_owned_after_tool_task(owned, kernel.clone())
                        }
                        result => {
                            let result = result.map(|advance| match advance {
                                WorkbenchRunAdvance::Complete(step) => step,
                                _ => unreachable!("owned execution yielded above"),
                            });
                            return Self::begin_owned_finalization(behavior, kernel, owned, result);
                        }
                    };
                    Ok(WorkbenchAdvance::Park(task))
                })
            })
        }))
    }

    fn owned_unit_task(mut owned: OwnedExecution<H, O>) -> OwnedWorkbenchTask<Self> {
        let start = owned
            .state
            .cursor
            .starting
            .take()
            .expect("one native unit is captured before its checkout");
        Self::owned_step_task(
            owned,
            move |owned| {
                let context = owned.state.effects.context.clone();
                let workbench = owned
                    .workbench
                    .as_ref()
                    .expect("native unit retains original admitted workbench");
                Box::pin(begin_unit(workbench, context, start))
            },
            |_behavior, _kernel, mut owned, started| {
                assert!(
                    owned.state.cursor.started.is_none(),
                    "one native result per unit"
                );
                owned.state.cursor.started = Some(started);
                Ok(WorkbenchAdvance::Park(Self::continue_owned_task(owned)))
            },
        )
    }

    fn begin_owned_after_tool_task(mut owned: OwnedExecution<H, O>) -> OwnedWorkbenchTask<Self> {
        let budget = owned
            .state
            .cursor
            .after_tool
            .as_ref()
            .expect("original slot budget")
            .frame
            .remaining_display_budget();
        owned.state.cursor.unit.display_remaining =
            owned.state.cursor.unit.display_remaining.min(budget);
        Self::owned_step_task(
            owned,
            |owned| {
                let context = owned.state.effects.context.clone();
                let frame = &owned
                    .state
                    .cursor
                    .after_tool
                    .as_ref()
                    .expect("one admitted after-tool frame")
                    .frame;
                let workbench = owned
                    .workbench
                    .as_ref()
                    .expect("original admitted workbench");
                Box::pin(after_tool_wait::prepare(frame, workbench, context))
            },
            |_behavior, _kernel, mut owned, result| {
                owned
                    .state
                    .cursor
                    .after_tool
                    .as_mut()
                    .expect("same after-tool frame")
                    .prepared = Some(result);
                Ok(WorkbenchAdvance::Park(Self::continue_owned_task(owned)))
            },
        )
    }

    fn finish_owned_after_tool_task(
        &self,
        mut owned: OwnedExecution<H, O>,
        kernel: KernelContext,
    ) -> OwnedWorkbenchTask<Self> {
        let answer = owned
            .state
            .cursor
            .after_tool
            .as_mut()
            .expect("one completed after-tool invocation")
            .answer
            .take()
            .expect("one after-tool answer or timeout");
        Self::owned_step_task(
            owned,
            move |owned| {
                let context = owned.state.effects.context.clone();
                let registration = owned.cleanup.registration();
                let workbench = owned
                    .workbench
                    .as_ref()
                    .expect("original admitted workbench");
                let frame = &owned
                    .state
                    .cursor
                    .after_tool
                    .as_ref()
                    .expect("same after-tool frame")
                    .frame;
                Box::pin(async move {
                    if matches!(
                        &answer,
                        WorkbenchAfterToolAnswer::TimedOut
                            | WorkbenchAfterToolAnswer::Settled(Err(_))
                    ) {
                        let reason =
                            "after-tool slot did not complete its native continuation".to_owned();
                        let aborted = tokio::select! {
                            aborted = workbench.abort_owned_continuations(context.clone(), registration, reason) => aborted,
                            terminal = kernel.wait_requested_shutdown() => Err(ResidentActorWorkbenchError::ActorProtocol(
                                format!("after-tool cleanup remains unconfirmed during actor retirement: {}", terminal.summary)
                            )),
                        };
                        if let Err(error) = aborted {
                            return (answer, Err(error));
                        }
                    }
                    let binding = match &answer {
                        WorkbenchAfterToolAnswer::Settled(Ok(
                            crate::after_tool::Annotation::Pruned { .. },
                        )) => {
                            workbench
                                .bind_tool_result(
                                    context,
                                    frame.handle().to_owned(),
                                    frame.output().to_owned(),
                                )
                                .await
                        }
                        _ => Ok(()),
                    };
                    (answer, binding)
                })
            },
            |behavior, _kernel, mut owned, (answer, binding)| {
                let mut slot = owned
                    .state
                    .cursor
                    .after_tool
                    .take()
                    .expect("same after-tool invocation");
                owned.state.effects.after_tool_active = false;
                owned.state.cursor.running = None;
                // A failed cleanup is an execution failure: it cannot publish the
                // private writes or call the original result an acknowledged timeout.
                if let Err(error) = &binding {
                    if matches!(
                        &answer,
                        WorkbenchAfterToolAnswer::TimedOut
                            | WorkbenchAfterToolAnswer::Settled(Err(_))
                    ) {
                        slot.receipt.output = format!(
                            "{}\nAfter-tool cleanup remains unconfirmed: {error}",
                            slot.frame.output(),
                        );
                        slot.receipt.status = WorkbenchItemStatus::Stopped;
                        slot.receipt.failure_layer = Some(WorkbenchFailureLayer::Effect);
                        slot.receipt.operations =
                            std::mem::take(&mut owned.state.cursor.unit.operations);
                        owned.state.cursor.receipts.push(slot.receipt);
                        let failure = WorkbenchExecutionFailure {
                            receipts: std::mem::take(&mut owned.state.cursor.receipts),
                            failed_index: owned.state.cursor.index,
                            total: owned.state.request.items.len(),
                            source: binding.expect_err("failed exact cleanup"),
                        };
                        return Ok(WorkbenchAdvance::Park(Self::finish_owned_task(
                            owned,
                            Err(failure),
                        )));
                    }
                }
                slot.receipt.output =
                    behavior.render_owned_after_tool(&slot.frame, answer, binding);
                owned.state.cursor.completed = Some(slot.receipt);
                Ok(WorkbenchAdvance::Park(Self::continue_owned_task(owned)))
            },
        )
    }

    fn render_owned_after_tool(
        &mut self,
        frame: &after_tool_wait::AfterToolFrame,
        answer: WorkbenchAfterToolAnswer,
        binding: Result<(), ResidentActorWorkbenchError>,
    ) -> String {
        use crate::after_tool::{Annotation, Disposition, Invocation};
        let output = frame.output();
        let revision = frame.revision();
        let (delivered, disposition, detail) = match answer {
            WorkbenchAfterToolAnswer::TimedOut => {
                let wait = frame.deadline().duration_since(frame.entered());
                let reason = format!(
                    "no answer within {}",
                    crate::after_tool::describe_wait(wait)
                );
                let notice = self.after_tool.notice(frame.ordinal(), &reason);
                (
                    crate::after_tool::failed(output, &notice),
                    Disposition::TimedOut(wait),
                    reason,
                )
            }
            WorkbenchAfterToolAnswer::Settled(Err(error))
                if error.is_observation_budget_exhausted() =>
            {
                (
                    output.to_owned(),
                    Disposition::Abstained(
                        "tool result too large for the slot to relay through its own effects"
                            .into(),
                    ),
                    String::new(),
                )
            }
            WorkbenchAfterToolAnswer::Settled(Err(error)) => {
                let reason = crate::after_tool::compact_reason(&error.to_string());
                let notice = self.after_tool.notice(frame.ordinal(), &reason);
                (
                    crate::after_tool::failed(output, &notice),
                    Disposition::Failed(reason.clone()),
                    reason,
                )
            }
            WorkbenchAfterToolAnswer::Settled(Ok(Annotation::Nothing)) => {
                (output.to_owned(), Disposition::Silent, String::new())
            }
            WorkbenchAfterToolAnswer::Settled(Ok(Annotation::Abstained(reason))) => (
                output.to_owned(),
                Disposition::Abstained(reason.clone()),
                reason,
            ),
            WorkbenchAfterToolAnswer::Settled(Ok(Annotation::Annotated(text))) => (
                crate::after_tool::annotated(output, &text, revision),
                Disposition::Annotated,
                crate::after_tool::compact_reason(&text),
            ),
            WorkbenchAfterToolAnswer::Settled(Ok(Annotation::Pruned { text, .. })) => match binding
            {
                Ok(()) => (
                    crate::after_tool::pruned(&text, frame.handle(), revision),
                    Disposition::Pruned(frame.handle().into()),
                    crate::after_tool::compact_reason(&text),
                ),
                Err(error) => {
                    let reason = crate::after_tool::compact_reason(&error.to_string());
                    let notice = self.after_tool.notice(frame.ordinal(), &reason);
                    (
                        crate::after_tool::failed(output, &notice),
                        Disposition::Failed(reason.clone()),
                        reason,
                    )
                }
            },
        };
        let elapsed = frame.entered().elapsed();
        tracing::info!(actor = %frame.tools().actor(), tool = %frame.call().name,
            ordinal = frame.ordinal(), elapsed_ms = elapsed.as_millis(),
            disposition = ?disposition, detail = %detail, "after-tool slot invoked");
        self.after_tool.record(Invocation {
            ordinal: frame.ordinal(),
            tool: frame.call().name.clone(),
            elapsed,
            provenance: frame.provenance().into(),
            disposition,
        });
        delivered
    }

    fn owned_fragment_task(&self, mut owned: OwnedExecution<H, O>) -> OwnedWorkbenchTask<Self> {
        let request = owned
            .state
            .cursor
            .running
            .as_mut()
            .expect("native advance retains its original fragment cursor")
            .native_start
            .take()
            .expect("one captured native advance");
        let runner = self.environment.runner.clone();
        Self::owned_step_task(
            owned,
            move |owned| {
                let context = owned.state.effects.context.clone();
                let workbench = owned
                    .workbench
                    .as_ref()
                    .expect("native advance retains original admitted workbench");
                Box::pin(
                    async move { advance_fragment(workbench, &runner, context, request).await },
                )
            },
            |_behavior, _kernel, mut owned, result| {
                let current = owned
                    .state
                    .cursor
                    .running
                    .as_mut()
                    .expect("same native fragment cursor after fenced completion");
                assert!(
                    current.native_result.is_none(),
                    "one native result per advance"
                );
                current.native_result = Some(result);
                Ok(WorkbenchAdvance::Park(Self::continue_owned_task(owned)))
            },
        )
    }

    fn owned_effect_task(
        &self,
        mut owned: OwnedExecution<H, O>,
        kernel: KernelContext,
    ) -> OwnedWorkbenchTask<Self> {
        let pending = owned
            .state
            .cursor
            .running
            .as_mut()
            .expect("effect wait retains running fragment")
            .parked_effect
            .take()
            .expect("one captured effect wait");
        let pending = match pending.wait {
            OwnedWorkbenchWait::DeferredCommit(prepared) => {
                let environment = self.environment.clone();
                let readiness_kernel = kernel.clone();
                return Self::owned_step_task(
                    owned,
                    move |_| {
                        Box::pin(captured_commit::await_ready_deferred(
                            environment,
                            readiness_kernel,
                            prepared,
                        ))
                    },
                    move |behavior, kernel, owned, ready| {
                        let scopes = behavior.apply_ready_deferred_commit(kernel, ready);
                        let environment = behavior.environment.clone();
                        Ok(WorkbenchAdvance::Park(Self::owned_step_task(
                            owned,
                            move |_| Box::pin(captured_commit::await_scopes(environment, scopes)),
                            move |behavior, kernel, owned, finalized| {
                                let release =
                                    behavior.apply_finalized_deferred_commit(kernel, finalized);
                                let environment = behavior.environment.clone();
                                let release_kernel = kernel.clone();
                                Ok(WorkbenchAdvance::Park(Self::owned_step_task(
                                    owned,
                                    move |_| {
                                        Box::pin(captured_commit::await_release(
                                            environment,
                                            release_kernel,
                                            release,
                                        ))
                                    },
                                    move |behavior, kernel, owned, completed| {
                                        let resume = behavior.settle_captured_commit(completed);
                                        let operation = Box::pin(captured_commit::resume(
                                            behavior.environment.clone(),
                                            kernel.clone(),
                                            resume,
                                        ));
                                        let pending = ParkedWorkbenchEffect {
                                            wait: OwnedWorkbenchWait::Prepared(operation),
                                            ordinal: pending.ordinal,
                                            effect: pending.effect,
                                            started: pending.started,
                                        };
                                        Ok(WorkbenchAdvance::Park(
                                            behavior.resume_owned_effect_task(
                                                owned,
                                                kernel.clone(),
                                                pending,
                                            ),
                                        ))
                                    },
                                )))
                            },
                        )))
                    },
                );
            }
            OwnedWorkbenchWait::CapturedCommit(prepared) => {
                let environment = self.environment.clone();
                let readiness_kernel = kernel.clone();
                return Self::owned_step_task(
                    owned,
                    move |_| {
                        Box::pin(captured_commit::await_ready(
                            environment,
                            readiness_kernel,
                            prepared,
                        ))
                    },
                    move |behavior, kernel, owned, ready| {
                        let release = behavior.apply_captured_commit(kernel, ready);
                        let environment = behavior.environment.clone();
                        let release_kernel = kernel.clone();
                        Ok(WorkbenchAdvance::Park(Self::owned_step_task(
                            owned,
                            move |_| {
                                Box::pin(captured_commit::await_release(
                                    environment,
                                    release_kernel,
                                    release,
                                ))
                            },
                            move |behavior, kernel, owned, completed| {
                                let resume = behavior.settle_captured_commit(completed);
                                let operation = Box::pin(captured_commit::resume(
                                    behavior.environment.clone(),
                                    kernel.clone(),
                                    resume,
                                ));
                                let pending = ParkedWorkbenchEffect {
                                    wait: OwnedWorkbenchWait::Prepared(operation),
                                    ordinal: pending.ordinal,
                                    effect: pending.effect,
                                    started: pending.started,
                                };
                                Ok(WorkbenchAdvance::Park(behavior.resume_owned_effect_task(
                                    owned,
                                    kernel.clone(),
                                    pending,
                                )))
                            },
                        )))
                    },
                );
            }
            OwnedWorkbenchWait::Launch(prepared) => {
                let environment = self.environment.clone();
                let launch_kernel = kernel.clone();
                return Self::owned_step_task(
                    owned,
                    move |_| {
                        Box::pin(child_launch::await_launch(
                            environment,
                            launch_kernel,
                            prepared,
                        ))
                    },
                    move |behavior, kernel, owned, completed| {
                        let resume = behavior.apply_child_launch(kernel, completed);
                        let operation = Box::pin(child_launch::resume_launch(
                            behavior.environment.clone(),
                            kernel.clone(),
                            resume,
                        ));
                        let pending = ParkedWorkbenchEffect {
                            wait: OwnedWorkbenchWait::Prepared(operation),
                            ordinal: pending.ordinal,
                            effect: pending.effect,
                            started: pending.started,
                        };
                        Ok(WorkbenchAdvance::Park(behavior.resume_owned_effect_task(
                            owned,
                            kernel.clone(),
                            pending,
                        )))
                    },
                );
            }
            wait => ParkedWorkbenchEffect { wait, ..pending },
        };
        if let OwnedWorkbenchWait::Command {
            request: crate::generated::commands::CommandsReq::CommandPresentWith(job, presentation),
            ..
        } = &pending.wait
        {
            let named_tool = owned.state.request.tool_call().is_some();
            let request = command_presentation::CommandPresentationRequest {
                job: job.clone(),
                presentation: presentation.clone(),
                summarize: !named_tool
                    && owned
                        .state
                        .cursor
                        .running
                        .as_ref()
                        .expect("same presentation fragment")
                        .fragment
                        .as_ref()
                        .expect("presentation retains its fragment")
                        .summarizes_bound_commands(),
                named_tool,
                display_remaining: owned.state.cursor.unit.display_remaining,
            };
            let environment = self.environment.clone();
            let permitted = self
                .descriptor
                .effective_role()
                .effect_keys()
                .contains(&crate::ActorEffectKey::Commands);
            return OwnedWorkbenchTask::new(Box::pin(async move {
                let (timing, cleanup) = owned.scopes();
                let cancel = owned
                    .state
                    .effects
                    .control
                    .as_ref()
                    .expect("original cancellation owner")
                    .native_cancel();
                let prepared = crate::resident_workbench::with_invocation_cancellation(
                    cancel,
                    timing.scope(
                        cleanup.scope(command_presentation::prepare(
                            &environment.commands,
                            &owned.state.effects.context,
                            owned
                                .workbench
                                .as_ref()
                                .expect("prepared execution has its workbench"),
                            request,
                            permitted,
                        )),
                    ),
                )
                .await;
                OwnedWorkbenchCompletion::advance(move |behavior: &mut Self, _kernel| {
                    let (timing, cleanup) = owned.scopes();
                    timing.sync_scope(|| {
                        cleanup.sync_scope(|| {
                            let result = prepared.and_then(|prepared| {
                                prepared.apply(
                                    &environment.commands,
                                    owned.state.effects.context.actor,
                                    owned
                                        .state
                                        .cursor
                                        .running
                                        .as_mut()
                                        .expect("same presentation fragment")
                                        .fragment
                                        .as_mut()
                                        .expect("presentation retains its fragment"),
                                    &mut owned.state.cursor.unit.display_remaining,
                                    &mut owned.state.cursor.unit.command_output,
                                )
                            });
                            if let Err(error) = result {
                                owned
                                    .state
                                    .cursor
                                    .running
                                    .as_mut()
                                    .expect("same effect fragment")
                                    .inflight_effect = None;
                                record_workbench_operation(
                                    &mut owned.state.cursor.unit.operations,
                                    owned.state.request.execution_id(),
                                    owned.state.cursor.index,
                                    pending.ordinal,
                                    &pending.effect,
                                    pending.started.elapsed(),
                                    WorkbenchOperationDisposition::Unknown,
                                );
                                owned
                                    .state
                                    .cursor
                                    .running
                                    .as_mut()
                                    .expect("same presentation fragment")
                                    .resume_failure = Some(error);
                                return Ok(WorkbenchAdvance::Park(Self::continue_owned_task(
                                    owned,
                                )));
                            }
                            // Rendering, display budget and page receipt advance together
                            // before the native continuation receives this acknowledgement.
                            Ok(WorkbenchAdvance::Park(
                                behavior.resume_owned_effect_task(owned, kernel, pending),
                            ))
                        })
                    })
                })
            }));
        }
        self.resume_owned_effect_task(owned, kernel, pending)
    }

    fn resume_owned_effect_task(
        &self,
        owned: OwnedExecution<H, O>,
        kernel: KernelContext,
        pending: ParkedWorkbenchEffect,
    ) -> OwnedWorkbenchTask<Self> {
        let environment = self.environment.clone();
        let commands_permitted = self
            .descriptor
            .effective_role()
            .effect_keys()
            .contains(&crate::ActorEffectKey::Commands);
        let context = owned.state.effects.context.clone();
        let control = owned
            .state
            .effects
            .control
            .clone()
            .unwrap_or_else(crate::WorkbenchExecutionControl::untracked);
        let invocation = owned.state.effects.invocation_work.clone();
        let observed_child = pending.wait.observe_after_resume();
        Self::owned_step_task(
            owned,
            move |_| {
                Box::pin(await_effect(
                    environment,
                    kernel,
                    context,
                    control,
                    pending.wait,
                    commands_permitted,
                    invocation,
                ))
            },
            move |behavior, _kernel, mut owned, result| {
                owned
                    .state
                    .cursor
                    .running
                    .as_mut()
                    .expect("same effect fragment")
                    .inflight_effect = None;
                record_workbench_operation(
                    &mut owned.state.cursor.unit.operations,
                    owned.state.request.execution_id(),
                    owned.state.cursor.index,
                    pending.ordinal,
                    &pending.effect,
                    pending.started.elapsed(),
                    result.disposition,
                );
                if let Some(job) = result.started_job {
                    owned
                        .state
                        .cursor
                        .running
                        .as_mut()
                        .expect("same command fragment")
                        .fragment
                        .as_mut()
                        .expect("captured command owns its fragment")
                        .record_started_job(job);
                }
                let current = owned
                    .state
                    .cursor
                    .running
                    .as_mut()
                    .expect("same effect fragment");
                match result.outcome {
                    Ok(outcome) => {
                        if let Some(child) = observed_child {
                            behavior.record_child_observation(child);
                        }
                        current.outcome = Some(outcome);
                    }
                    Err(error) => current.resume_failure = Some(error),
                }
                Ok(WorkbenchAdvance::Park(Self::continue_owned_task(owned)))
            },
        )
    }

    fn finish_owned_task(
        owned: OwnedExecution<H, O>,
        result: Result<KernelStep<WorkbenchResponse>, WorkbenchExecutionFailure>,
    ) -> OwnedWorkbenchTask<Self> {
        OwnedWorkbenchTask::new(Box::pin(async move {
            OwnedWorkbenchCompletion::advance(move |behavior: &mut Self, kernel| {
                Self::begin_owned_finalization(behavior, kernel, owned, result)
            })
        }))
    }

    fn begin_owned_finalization(
        behavior: &mut Self,
        kernel: &KernelContext,
        owned: OwnedExecution<H, O>,
        result: Result<KernelStep<WorkbenchResponse>, WorkbenchExecutionFailure>,
    ) -> Result<WorkbenchAdvance<Self>, KernelInvocationFailure> {
        owned.state.effects.invocation_work.close();
        if owned.private.is_some() && private_publication_required(&result) {
            return Ok(WorkbenchAdvance::Park(Self::publish_owned_execution_task(
                owned,
                behavior.environment.clone(),
                kernel.clone(),
                result,
            )));
        }
        owned
            .state
            .effects
            .control
            .as_ref()
            .expect("owned execution retains its original control")
            .publication_decision()
            .terminate();
        Self::settle_owned_execution(behavior, kernel, owned, result)
    }

    fn settle_owned_execution(
        behavior: &mut Self,
        kernel: &KernelContext,
        owned: OwnedExecution<H, O>,
        result: Result<KernelStep<WorkbenchResponse>, WorkbenchExecutionFailure>,
    ) -> Result<WorkbenchAdvance<Self>, KernelInvocationFailure> {
        let (timing, cleanup) = owned.scopes();
        timing.sync_scope(|| {
            cleanup.sync_scope(|| {
                let finalization =
                    behavior.begin_workbench_finalization(&owned.state, kernel, result);
                Ok(WorkbenchAdvance::Park(
                    Self::settle_owned_finalization_task(
                        owned,
                        behavior.environment.clone(),
                        finalization,
                    ),
                ))
            })
        })
    }

    fn publish_owned_execution_task(
        owned: OwnedExecution<H, O>,
        environment: ResidentEnvironment<H, O>,
        kernel: KernelContext,
        result: Result<KernelStep<WorkbenchResponse>, WorkbenchExecutionFailure>,
    ) -> OwnedWorkbenchTask<Self> {
        let runner = environment.runner.clone();
        Self::owned_step_task(
            owned,
            move |owned| {
                let private = owned
                    .private
                    .as_ref()
                    .expect("publication retains original private admission")
                    .clone();
                let context = owned.state.effects.context.clone();
                Box::pin(async move {
                    let actor = context.actor;
                    tracing::info!(target: "exomonad_actor::workbench_phase", actor = %actor, phase = "private_publish_started", "workbench phase");
                    let published = runner.publish_private_execution(context, private).await;
                    tracing::info!(target: "exomonad_actor::workbench_phase", actor = %actor, phase = "private_publish_completed", "workbench phase");
                    published
                })
            },
            move |behavior, _kernel, owned, published| {
                use crate::resident_workbench::PrivateExecutionPublication;
                use tidepool_runtime::session::PublicManifestCommit;
                match published {
                    Ok(PrivateExecutionPublication::Manifest(
                        PublicManifestCommit::Durable | PublicManifestCommit::Ephemeral,
                    )) => Self::settle_owned_execution(behavior, &kernel, owned, result),
                    Ok(PrivateExecutionPublication::Manifest(
                        PublicManifestCommit::PublishedDurabilityUnconfirmed { detail },
                    )) => Ok(WorkbenchAdvance::Park(
                        Self::confirm_owned_publication_task(
                            owned,
                            environment,
                            kernel,
                            result,
                            detail,
                        ),
                    )),
                    failure => {
                        let error = match failure {
                            Err(error) => error,
                            Ok(PrivateExecutionPublication::Rejected { reason, diagnostic }) => {
                                ResidentActorWorkbenchError::ActorProtocol(format!(
                                    "private publication rejected: {reason:?}: {diagnostic}"
                                ))
                            }
                            Ok(PrivateExecutionPublication::Manifest(commit)) => {
                                ResidentActorWorkbenchError::ActorProtocol(format!(
                                    "private publication did not commit: {commit:?}"
                                ))
                            }
                        };
                        owned
                            .state
                            .effects
                            .control
                            .as_ref()
                            .expect("owned execution retains its original control")
                            .publication_decision()
                            .terminate();
                        Self::settle_owned_execution(
                            behavior,
                            &kernel,
                            owned,
                            Err(private_publication_failure(result, error)),
                        )
                    }
                }
            },
        )
    }

    fn confirm_owned_publication_task(
        owned: OwnedExecution<H, O>,
        environment: ResidentEnvironment<H, O>,
        kernel: KernelContext,
        result: Result<KernelStep<WorkbenchResponse>, WorkbenchExecutionFailure>,
        detail: String,
    ) -> OwnedWorkbenchTask<Self> {
        Self::owned_step_task(
            owned,
            move |owned| {
                let context = owned.state.effects.context.clone();
                Box::pin(async move {
                    // The existing atomic-write owner retains the visible write.
                    // This task attempts confirmation once, never execution or staging.
                    tokio::select! {
                        confirmed = environment.runner.confirm_publication_durability(context) => confirmed,
                        terminal = kernel.wait_requested_shutdown() => Err(
                            ResidentActorWorkbenchError::ActorProtocol(format!(
                                "published write durability remains unconfirmed during actor retirement: {}",
                                terminal.summary,
                            ))
                        ),
                    }
                })
            },
            move |behavior, kernel, owned, confirmed| {
                let result = match confirmed {
                    Ok(()) => result,
                    Err(error) => Err(private_publication_failure(
                        result,
                        ResidentActorWorkbenchError::ActorProtocol(format!(
                            "published write durability remains unconfirmed: {detail}; {error}"
                        )),
                    )),
                };
                Self::settle_owned_execution(behavior, kernel, owned, result)
            },
        )
    }

    fn settle_owned_finalization_task(
        owned: OwnedExecution<H, O>,
        environment: ResidentEnvironment<H, O>,
        finalization: WorkbenchFinalization,
    ) -> OwnedWorkbenchTask<Self> {
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
                        let cleanup = owned
                            .workbench
                            .as_ref()
                            .expect("cancelled execution retains its native cleanup owner")
                            .abort_owned_continuations(
                                owned.state.effects.context.clone(),
                                owned.cleanup.registration(),
                                "execution cancelled before settlement".into(),
                            )
                            .await;
                        if let Err(error) = cleanup {
                            owned
                                .state
                                .effects
                                .control
                                .as_ref()
                                .expect("original cancellation owner")
                                .mark_unconfirmed();
                            return Err(KernelInvocationFailure::Failed {
                                actor: owned.state.effects.context.actor,
                                detail: format!("cancelled execution cleanup unconfirmed: {error}"),
                            });
                        }
                        owned
                            .state
                            .effects
                            .control
                            .as_ref()
                            .expect("original cancellation owner")
                            .acknowledge_cancellation();
                    }
                    settle_workbench_finalization(environment, finalization).await
                })
            },
            |behavior, _kernel, mut owned, result| {
                let result = behavior.complete_workbench_finalization(&mut owned.state, result);
                let outcome = match &result {
                    Ok(
                        KernelStep::Continue(response)
                        | KernelStep::ContinueLater(response)
                        | KernelStep::Stop {
                            output: response, ..
                        },
                    ) => format!("{:?}", response.status),
                    Err(_) => "error".into(),
                };
                owned
                    .timing
                    .take()
                    .expect("one terminal timing owner")
                    .finish(&outcome);
                // Exact resources and continuation custody cross the last fence.
                let _owned = owned;
                result.map(WorkbenchAdvance::Complete)
            },
        )
    }
}

fn private_publication_required(
    result: &Result<KernelStep<WorkbenchResponse>, WorkbenchExecutionFailure>,
) -> bool {
    let response = match result {
        Ok(KernelStep::Continue(response) | KernelStep::ContinueLater(response))
        | Ok(KernelStep::Stop {
            output: response, ..
        }) => response,
        Err(_) => return false,
    };
    !matches!(
        response.status,
        WorkbenchRunStatus::Rejected | WorkbenchRunStatus::RequestCancelled
    )
}

fn private_publication_failure(
    result: Result<KernelStep<WorkbenchResponse>, WorkbenchExecutionFailure>,
    source: ResidentActorWorkbenchError,
) -> WorkbenchExecutionFailure {
    match result {
        Ok(KernelStep::Continue(response) | KernelStep::ContinueLater(response))
        | Ok(KernelStep::Stop {
            output: response, ..
        }) => WorkbenchExecutionFailure {
            receipts: response.items,
            failed_index: response.next_index.saturating_sub(1),
            total: response.total,
            source,
        },
        Err(failure) => WorkbenchExecutionFailure { source, ..failure },
    }
}

fn terminal_task<B: 'static>(
    result: Result<KernelStep<WorkbenchResponse>, KernelInvocationFailure>,
) -> WorkbenchDispatch<B> {
    WorkbenchDispatch::Owned(OwnedWorkbenchTask::new(Box::pin(async move {
        OwnedWorkbenchCompletion::new(move |_| result)
    })))
}

async fn await_effect<H, O>(
    environment: ResidentEnvironment<H, O>,
    kernel: KernelContext,
    context: ActorSessionContext,
    control: Arc<crate::WorkbenchExecutionControl>,
    wait: OwnedWorkbenchWait,
    commands_permitted: bool,
    invocation: Arc<InvocationWork>,
) -> commands::CommandResolution
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    let result = match wait {
        OwnedWorkbenchWait::Launch(_)
        | OwnedWorkbenchWait::CapturedCommit(_)
        | OwnedWorkbenchWait::DeferredCommit(_) => {
            unreachable!("child launch requires fenced actor application")
        }
        OwnedWorkbenchWait::Prepared(operation) => operation.await,
        OwnedWorkbenchWait::Drain {
            continuation,
            target,
        } => {
            return drain_wait::await_drain(
                environment,
                kernel,
                context,
                control,
                continuation,
                target,
            )
            .await;
        }
        OwnedWorkbenchWait::Exit {
            continuation,
            terminal,
        } => {
            terminal_wait::await_exit(
                environment,
                kernel,
                context,
                control,
                continuation,
                terminal,
            )
            .await
        }
        OwnedWorkbenchWait::PollExit {
            continuation,
            terminal,
            ..
        } => {
            environment
                .runner
                .resume_optional_terminal(context, continuation, terminal)
                .await
        }
        OwnedWorkbenchWait::Watch(poll) => {
            request_wait::await_watch(environment, kernel, context, control, poll).await
        }
        OwnedWorkbenchWait::Sleep {
            continuation,
            duration,
        } => {
            clock_wait::await_sleep(
                environment,
                kernel,
                context,
                control,
                continuation,
                duration,
            )
            .await
        }
        OwnedWorkbenchWait::External { continuation, work } => {
            await_external(
                environment,
                kernel,
                context,
                Some(control),
                continuation,
                work,
            )
            .await
        }
        OwnedWorkbenchWait::Jev {
            continuation,
            request,
        } => {
            ask_jev(
                environment,
                kernel,
                context,
                Some(control),
                continuation,
                request,
            )
            .await
        }
        OwnedWorkbenchWait::Command {
            continuation,
            request,
        } => {
            let exec_started = std::time::Instant::now();
            let wait_control = (commands_permitted && commands::waits_for_completion(&request))
                .then_some(control.as_ref());
            let result = commands::resolve_command(
                &environment,
                &kernel,
                &context,
                continuation,
                request,
                commands_permitted,
                wait_control,
                Some(invocation.as_ref()),
            )
            .await;
            crate::call_timing::add_exec_ms(exec_started.elapsed().as_millis());
            return result;
        }
    };
    commands::CommandResolution {
        disposition: match &result {
            Ok(_) => WorkbenchOperationDisposition::Committed,
            Err(error) => disposition_for_non_command_failure(error),
        },
        outcome: result,
        started_job: None,
    }
}

pub(super) async fn await_external<H, O>(
    environment: ResidentEnvironment<H, O>,
    kernel: KernelContext,
    context: ActorSessionContext,
    control: Option<Arc<crate::WorkbenchExecutionControl>>,
    continuation: ResidentHole,
    work: tidepool_effect::DeferredEffect,
) -> Result<ResidentOutcome, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    let cancellation = work.cancellation_signal();
    let running = environment
        .runner
        .run_external(context.clone(), continuation, work);
    tokio::pin!(running);
    tokio::select! {
        outcome = &mut running => outcome,
        _ = async {
            match &control {
                Some(control) => control.wait_for_cancellation().await,
                None => std::future::pending().await,
            }
        } => {
            if let Some(cancellation) = cancellation { cancellation.request(); }
            // Cancellation requests do not prove owner settlement.
            running.await
        }
        _ = kernel.wait_requested_shutdown() => {
            if let Some(control) = &control { control.request_cancellation(); }
            if let Some(cancellation) = cancellation { cancellation.request(); }
            running.await
        }
    }
}

pub(super) async fn ask_jev<H, O>(
    environment: ResidentEnvironment<H, O>,
    kernel: KernelContext,
    context: ActorSessionContext,
    control: Option<Arc<crate::WorkbenchExecutionControl>>,
    continuation: ResidentHole,
    request: String,
) -> Result<ResidentOutcome, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    let backend = Arc::clone(&environment.jev);
    // The packet is opaque JSON from here: `Jev.Operators` on the
    // Haskell side already carries the questions, labels and
    // (on the way back) likelihoods. Full bodies are debug-only
    // and bounded; `info` stays one compact line either way.
    tracing::debug!(
        actor = %context.actor,
        packet = %crate::workbench_display::bounded_output(&request, 4096),
        "jev call packet"
    );
    let started = std::time::Instant::now();
    let asking = backend.ask(request);
    tokio::pin!(asking);
    let answer = tokio::select! {
        answer = &mut asking => answer,
        _ = kernel.wait_requested_shutdown() => {
            if let Some(control) = &control { control.request_cancellation(); }
            asking.await
        }
        _ = async {
            match &control {
                Some(control) => control.wait_for_cancellation().await,
                None => std::future::pending().await,
            }
        } => asking.await,
    };
    let elapsed_ms = started.elapsed().as_millis();
    crate::call_timing::add_jev_ms(elapsed_ms);
    match &answer {
        Ok(body) => {
            tracing::debug!(
                actor = %context.actor,
                answer = %crate::workbench_display::bounded_output(body, 4096),
                "jev call answer"
            );
            tracing::info!(actor = %context.actor, elapsed_ms, "jev call answered");
        }
        Err(failure) => {
            tracing::info!(
                actor = %context.actor,
                ?failure,
                elapsed_ms,
                "jev call failed"
            );
        }
    }
    environment
        .runner
        .resume_value(context.clone(), continuation, answer)
        .await
}

#[cfg(test)]
mod authority_tests {
    use super::WorkbenchCompilationAuthority;
    use std::{path::PathBuf, sync::Arc};

    struct SourceOwner {
        identities: Vec<String>,
        roots: Vec<PathBuf>,
    }

    impl crate::RetainedSourceLayer for SourceOwner {
        fn identities(&self) -> &[String] {
            &self.identities
        }

        fn include_paths(&self) -> &[PathBuf] {
            &self.roots
        }
    }

    struct SourceService(crate::SourceLayerIssuer);

    impl crate::ActorSourceLayers for SourceService {
        fn validate_source_authority(
            &self,
            source: &crate::CheckpointSourceLayer,
        ) -> Result<(), String> {
            self.0
                .owns(source)
                .then_some(())
                .ok_or_else(|| "foreign source issuer".into())
        }

        fn layer_include(&self, _: &[String]) -> Result<Vec<PathBuf>, String> {
            Ok(Vec::new())
        }

        fn bind(&self, _: tidepool_repr::PrincipalId, _: &[String]) {}
    }

    fn context() -> crate::ActorSessionContext {
        crate::ActorSessionContext {
            actor: crate::ActorRef::first(crate::ActorId(8)),
            placement: crate::ActorPlacement {
                session: tidepool_repr::SessionId(1),
                resource_scope: tidepool_codegen::suspension::RealmId(0),
                lexical_scope: tidepool_codegen::scope::ScopeId::ROOT,
            },
            effect_policy: Default::default(),
            live_payload: Default::default(),
            source_imports: Default::default(),
            haskell_effects_alias: String::new(),
            source_layer: Arc::from([PathBuf::from("original-root")]),
        }
    }

    #[test]
    fn admitted_authority_keeps_exact_source_owner_until_last_release() {
        let issuer = crate::SourceLayerIssuer::default();
        let owner = Arc::new(SourceOwner {
            identities: vec!["exact-revision".into()],
            roots: vec!["captured-root".into()],
        });
        let weak = Arc::downgrade(&owner);
        let source = issuer.issue(owner.clone());
        let tool = crate::InstalledToolLease::new(context().actor, source.clone(), None);
        let layers: crate::ActorSourceLayerResolver = Arc::new(SourceService(issuer));
        let (selected, authority) = WorkbenchCompilationAuthority::admit(
            context(),
            source.clone(),
            Some(tool),
            Some(&layers),
        )
        .unwrap_or_else(|error| panic!("admit owned source: {error:?}"));
        assert_eq!(selected.source_layer.as_ref(), source.include_paths());
        assert_eq!(authority.source(), &source);
        assert_eq!(authority.installed_tools().unwrap().source(), &source);
        assert_ne!(authority.authority_digest(), [0; 32]);
        drop(owner);
        drop(source);
        drop(layers);
        assert!(weak.upgrade().is_some());
        drop(authority);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn authority_refuses_foreign_issuer_even_with_identical_source_observations() {
        let owner = Arc::new(SourceOwner {
            identities: vec!["same-revision".into()],
            roots: vec!["same-root".into()],
        });
        let foreign = crate::SourceLayerIssuer::default().issue(owner);
        let layers: crate::ActorSourceLayerResolver =
            Arc::new(SourceService(crate::SourceLayerIssuer::default()));
        assert!(
            WorkbenchCompilationAuthority::admit(context(), foreign, None, Some(&layers),).is_err()
        );
    }

    #[test]
    fn authority_refuses_unowned_roots_and_foreign_tool_incarnation() {
        assert!(
            WorkbenchCompilationAuthority::admit(context(), Default::default(), None, None,)
                .is_err()
        );
        let mut empty = context();
        empty.source_layer = Arc::from([]);
        let tool = crate::InstalledToolLease::new(
            crate::ActorRef::first(crate::ActorId(9)),
            Default::default(),
            None,
        );
        assert!(
            WorkbenchCompilationAuthority::admit(empty, Default::default(), Some(tool), None,)
                .is_err()
        );
    }
}
