//! Single-admission preparation and watch tasks retain one execution cursor.

use tidepool_runtime::session::WorkbenchNotPublishedReason;

mod reload;

use super::*;
use crate::{OwnedWorkbenchCompletion, OwnedWorkbenchTask, WorkbenchAdvance, WorkbenchDispatch};

#[cfg(test)]
mod model_tests;
#[cfg(test)]
mod publication_tests;

/// Source and tool owners admitted once for this hosted execution. Compiler
/// recipe observations are added by the workbench's original snapshot owner.
pub(crate) struct WorkbenchCompilationAuthority {
    source: crate::CheckpointSourceLayer,
    source_layers: Option<crate::ActorSourceLayerResolver>,
    // Keep the admitted implementation alive until this execution releases it.
    _installed_tools: Option<crate::InstalledToolLease>,
    authority_digest: [u8; 32],
}

impl WorkbenchCompilationAuthority {
    #[cfg(test)]
    pub(crate) fn for_test(context: ActorSessionContext) -> Arc<Self> {
        Self::admit(context, crate::CheckpointSourceLayer::default(), None, None)
            .expect("test actor has an unowned source baseline")
            .1
    }

    pub(super) fn admit(
        context: ActorSessionContext,
        source: crate::CheckpointSourceLayer,
        installed_tools: Option<crate::InstalledToolLease>,
        source_layers: Option<&crate::ActorSourceLayerResolver>,
    ) -> Result<(ActorSessionContext, Arc<Self>), KernelInvocationFailure> {
        let actor = context.actor;
        let reject = |detail| KernelInvocationFailure::Rejected {
            receipts: Vec::new(),
            actor,
            detail,
            diagnostic: None,
        };
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
        frame(context.haskell_effects_alias.as_bytes());
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
                source_layers: source_layers.cloned(),
                _installed_tools: installed_tools,
                authority_digest: *digest.finalize().as_bytes(),
            }),
        ))
    }

    pub(crate) fn source(&self) -> &crate::CheckpointSourceLayer {
        &self.source
    }

    pub(crate) fn toolset_source(&self) -> Result<crate::CheckpointSourceLayer, String> {
        match &self.source_layers {
            Some(layers) => layers.toolset_layer_from(&self.source),
            None if !self.source.is_owned() => Ok(crate::CheckpointSourceLayer::default()),
            None => Err("toolset preparation has no source owner".into()),
        }
    }

    pub(crate) fn fresh_toolset_source(
        &self,
        selected: &crate::CheckpointSourceLayer,
    ) -> Result<crate::CheckpointSourceLayer, String> {
        self.source_layers
            .as_ref()
            .ok_or("fresh installer preparation has no admitted source owner")?
            .fresh_toolset_layer_from(selected)
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
    readiness: Option<Arc<tidepool_runtime::session::RuntimeDurablePublicReadiness>>,
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
    pub(crate) fn issue(
        context: &ActorSessionContext,
        descriptor: &ActorDescriptor,
        readiness: Option<Arc<tidepool_runtime::session::RuntimeDurablePublicReadiness>>,
    ) -> Result<Arc<Self>, KernelInvocationFailure> {
        let refuse = |detail: &str| KernelInvocationFailure::Rejected {
            receipts: Vec::new(),
            actor: context.actor,
            detail: detail.into(),
            diagnostic: None,
        };
        if context.placement != descriptor.placement() {
            return Err(refuse(
                "publication owner differs from the admitted actor placement",
            ));
        }
        match (descriptor.persistence_policy(), readiness.as_ref()) {
            (crate::ActorPersistencePolicy::Ephemeral, None) => {}
            (crate::ActorPersistencePolicy::Durable, Some(native)) => {
                let owner = native.durable_owner();
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
                if native.session() != context.placement.session
                    || native.scope() != context.placement.lexical_scope
                    || !native.is_current()
                {
                    return Err(refuse(
                        "durable publication owner has no current native capability",
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
            readiness,
        }))
    }

    pub(crate) fn durable(&self) -> Option<&tidepool_runtime::session::RecoveryPublicOwner> {
        self.readiness.as_ref().map(|native| native.durable_owner())
    }
    pub(crate) fn is_ready(&self) -> bool {
        self.readiness
            .as_ref()
            .is_none_or(|native| native.is_ready())
    }

    pub(crate) fn is_current(&self) -> bool {
        self.readiness
            .as_ref()
            .is_none_or(|native| native.is_current())
    }

    pub(crate) fn matches_context(&self, context: &ActorSessionContext) -> bool {
        self.actor == context.actor && self.placement == context.placement
    }

    pub(crate) fn actor(&self) -> ActorRef {
        self.actor
    }
    pub(super) fn placement(&self) -> crate::ActorPlacement {
        self.placement
    }
}

struct OwnedExecution<H, O> {
    // Fenced steps transfer this allocation, including its linear boundary
    // custody. Polling and completion must not move the full cursor by value.
    state: Box<WorkbenchExecutionState>,
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
        if let Some(binding) = &self.state.effects.context_binding {
            binding.cancel();
        }
        if let Some(model) = &self.state.effects.model {
            model.cancel();
        }
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
            WorkbenchUnitStartRequest::Prepared { block, item } => {
                workbench
                    .begin_prepared_cell_item(context, block, item)
                    .await
            }
        }
    }
    .instrument(start.span)
    .await
}

pub(super) enum WorkbenchFragmentRequest {
    Scoped {
        fragment: ResidentWorkbenchFragment,
        outcome: ResidentOutcome,
        realm: RealmId,
    },
    ScopeStart {
        fragment: ResidentWorkbenchFragment,
        callback: RootCustody,
        realm: RealmId,
        token: i64,
        work: Arc<InvocationWork>,
    },
    ScopeResume {
        fragment: ResidentWorkbenchFragment,
        operation: futures_util::future::BoxFuture<
            'static,
            Result<ResidentOutcome, ResidentActorWorkbenchError>,
        >,
    },
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
    ScopeFailed {
        fragment: ResidentWorkbenchFragment,
        error: ResidentActorWorkbenchError,
    },
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
    let scoped = match request {
        WorkbenchFragmentRequest::Scoped {
            fragment,
            outcome,
            realm,
        } => Some((fragment, Ok(outcome), Some(realm))),
        WorkbenchFragmentRequest::ScopeStart {
            fragment,
            callback,
            realm,
            token,
            work,
        } => Some((
            fragment,
            runner
                .run_owned_scope_callback(context.clone(), callback, realm, token, work)
                .await,
            Some(realm),
        )),
        WorkbenchFragmentRequest::ScopeResume {
            fragment,
            operation,
        } => {
            return Ok(match operation.await {
                Ok(outcome) => WorkbenchFragmentAdvance::Captured {
                    fragment,
                    boundary: ResidentActorBoundary::ScopeResumed { outcome },
                },
                Err(error) => WorkbenchFragmentAdvance::ScopeFailed { fragment, error },
            });
        }
        request => return advance_unscoped(workbench, runner, context, request).await,
    };
    let (fragment, outcome, realm) = scoped.expect("scoped request");
    let boundary = match outcome {
        Ok(outcome) => {
            runner
                .capture_boundary(context, outcome, realm.expect("body realm"))
                .await
        }
        Err(error) => Err(error),
    };
    Ok(match boundary {
        Ok(boundary) => WorkbenchFragmentAdvance::Captured { fragment, boundary },
        Err(error) => WorkbenchFragmentAdvance::ScopeFailed { fragment, error },
    })
}

async fn advance_unscoped<H, O>(
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
        WorkbenchFragmentRequest::Scoped { .. }
        | WorkbenchFragmentRequest::ScopeStart { .. }
        | WorkbenchFragmentRequest::ScopeResume { .. } => {
            unreachable!("scoped request routed before fragment settlement")
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
                .capture_boundary(
                    context.clone(),
                    (*outcome).into(),
                    context.placement.resource_scope,
                )
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
                stamp.display,
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

async fn join_execution_step<T>(
    running: impl std::future::Future<Output = T>,
    control: Arc<crate::WorkbenchExecutionControl>,
    retirement: crate::RetainedActorExit,
    model: Option<Arc<dyn crate::CellModelBinding>>,
) -> T {
    tokio::pin!(running);
    tokio::select! {
        completed = &mut running => completed,
        _ = control.wait_for_cancellation() => {
            if let Some(model) = &model { model.cancel(); }
            running.await
        }
        _ = retirement.wait_requested_shutdown() => {
            if let Some(model) = &model { model.cancel(); }
            control.request_cancellation();
            running.await
        }
    }
}

async fn settle_execution_owners<H, O>(
    owned: &mut OwnedExecution<H, O>,
) -> Result<(), KernelInvocationFailure>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    let control = owned
        .state
        .effects
        .control
        .as_ref()
        .expect("owned execution retains its original control");
    let abort_requested = control.cancellation_requested();
    let native_cleanup = if abort_requested {
        owned
            .workbench
            .as_ref()
            .expect("cancelled execution retains its native cleanup owner")
            .abort_owned_continuations(
                owned.state.effects.context.clone(),
                owned.cleanup.registration(),
                "execution cancelled before settlement".into(),
            )
            .await
    } else {
        Ok(())
    };
    // Native waits may already have acknowledged consuming their own hole.
    // The cell's reply still waits for its model invocation owner, including
    // callbacks abandoned by authored failure or a normal early return.
    let model_cleanup = match &owned.state.effects.model {
        Some(model) => model.settle().await,
        None => Ok(()),
    };
    let failures = native_cleanup
        .err()
        .map(|error| format!("native cleanup unconfirmed: {error}"))
        .into_iter()
        .chain(
            model_cleanup
                .err()
                .map(|error| format!("model cleanup unconfirmed: {error}")),
        )
        .collect::<Vec<_>>();
    if !failures.is_empty() {
        control.mark_unconfirmed();
        return Err(KernelInvocationFailure::CleanupUnconfirmed {
            publication: None,
            receipts: Vec::new(),
            actor: owned.state.effects.context.actor,
            detail: failures.join("; "),
        });
    }
    if abort_requested && control.cancellation_requested() {
        control.acknowledge_cancellation();
    }
    Ok(())
}

async fn settle_execution_finalization<H, O>(
    owned: &mut OwnedExecution<H, O>,
    environment: ResidentEnvironment<H, O>,
    finalization: WorkbenchFinalization,
) -> WorkbenchFinalizationResult
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    let owners = settle_execution_owners(owned).await;
    // Invocation membership, reservations and checkpoint scopes keep their
    // own cleanup obligations even when native or model settlement refuses.
    let finalized = settle_workbench_finalization(environment, finalization).await;
    match owners {
        Ok(()) => finalized,
        Err(error) => {
            let (actor, detail) = match error {
                KernelInvocationFailure::CleanupUnconfirmed { actor, detail, .. } => {
                    (actor, detail)
                }
                other => (owned.state.effects.context.actor, other.to_string()),
            };
            retain_cleanup_failure(actor, detail, finalized)
        }
    }
}

pub(super) fn retain_cleanup_failure(
    actor: ActorRef,
    mut detail: String,
    finalized: WorkbenchFinalizationResult,
) -> WorkbenchFinalizationResult {
    let (receipts, publication) = match finalized.result {
        Ok(KernelStep::Continue(response) | KernelStep::ContinueLater(response))
        | Ok(KernelStep::Stop {
            output: response, ..
        }) => (response.items, response.publication),
        Err(error) => {
            detail.push_str(&format!("; workbench finalization: {error}"));
            (error.receipts().to_vec(), error.publication().cloned())
        }
    };
    WorkbenchFinalizationResult {
        result: Err(KernelInvocationFailure::CleanupUnconfirmed {
            publication,
            actor,
            detail,
            receipts,
        }),
        cleanup_confirmed: false,
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
            let model = owned.state.effects.model.clone();
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
                                        if let Some(model) = &model { model.cancel(); }
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
                join_execution_step(running, control.clone(), retirement, model.clone()).await
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
            context_binding,
            control,
            invocation,
            ..
        } = admitted;
        let inspection = request
            .tool_call()
            .filter(|call| call.name == crate::status_tool::STATUS_TOOL)
            .map(|call| crate::status_tool::parse(call.arguments.clone()));
        let reload = request.tool_call().is_some_and(|call| {
            matches!(
                call.name.as_str(),
                crate::reload_spec_tool::RELOAD_SPEC_TOOL
                    | crate::reload_helpers_tool::RELOAD_HELPERS_TOOL
            )
        });
        if inspection.is_none() && !reload && compilation_authority.is_none() {
            return terminal_task(Err(KernelInvocationFailure::Rejected {
                receipts: Vec::new(),
                actor: context.actor,
                detail: "authored execution has no compilation authority".into(),
                diagnostic: None,
            }));
        }
        let Some(workbench) = self.active_workbench() else {
            return terminal_task(Err(KernelInvocationFailure::Rejected {
                receipts: Vec::new(),
                actor: context.actor,
                detail: "actor application has no active Haskell workbench".into(),
                diagnostic: None,
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
        let model = if inspection.is_none() && !reload {
            self.environment.cell_model_factory.as_ref().map(|factory| {
                let RequestReservationOwner::Workbench { execution, .. } = &reservation_owner
                else {
                    unreachable!("workbench admission retains its execution identity")
                };
                factory.bind(
                    execution,
                    tidepool_repr::PrincipalId::from(context.actor),
                    &self.descriptor,
                )
            })
        } else {
            None
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
        let timing = crate::call_timing::CallScope::for_execution(
            kind,
            actor as u64,
            incarnation as u64,
            request.execution_id().cloned(),
        );
        let resources = match compilation_authority {
            Some(authority) => ExecutionResourceOwners::new(authority, public_owner),
            None => ExecutionResourceOwners::for_inspection(public_owner),
        };
        let control = Some(control.unwrap_or_else(crate::WorkbenchExecutionControl::untracked));
        let display_receipt_owner = request.execution_id().and_then(|execution| {
            self.workbench_executions
                .lock()
                .display_receipt_owner(execution, invocation.as_ref())
        });
        if let Some(owner) = &display_receipt_owner {
            control
                .as_ref()
                .expect("original execution control")
                .bind_receipt_owner(owner.clone());
        }
        let cleanup = workbench
            .continuation_cleanup_owner(
                context.clone(),
                "hosted execution abandoned before exact continuation settlement".into(),
                resources.clone(),
            )
            .with_compiler_owner(crate::resident_workbench::CompilerCloseOwner::Invocation {
                work: invocation_work.clone(),
                control: control.clone(),
            });
        let owned = OwnedExecution {
            observation: self.runtime_observation.clone(),
            retirement: kernel.retained_exit(),
            state: Box::new(WorkbenchExecutionState {
                effects: WorkbenchEffectState {
                    display_receipt_owner,
                    park_effects: true,
                    context: context.clone(),
                    public_visibility: None,
                    control,
                    model,
                    context_binding,
                    installed_tools,
                    admitted_source,
                    reservation_owner,
                    invocation_work,
                    publication: CheckpointPublication::Workbench {
                        boundary: request.checkpoint_boundary().cloned(),
                        capture,
                    },
                    after_tool_active: false,
                    terminal_transfer: None,
                },
                request,
                replay_request,
                invocation,
                cursor: WorkbenchCursor::default(),
            }),
            workbench: Some(workbench),
            private: None,
            timing: Some(timing),
            cleanup,
            resources,
        };
        if reload {
            return WorkbenchDispatch::Owned(self.owned_reload_task(owned));
        }
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
                        let model = owned.state.effects.model.clone();
                        let control = owned
                            .state
                            .effects
                            .control
                            .clone()
                            .expect("original cancellation owner");
                        let retirement = owned.retirement.clone();
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
                        tokio::select! {
                            biased;
                            result = &mut advance => Some(result),
                            _ = control.wait_for_cancellation() => {
                                if let Some(model) = &model { model.cancel(); }
                                Some(advance.await)
                            },
                            _ = retirement.wait_requested_shutdown() => {
                                if let Some(model) = &model { model.cancel(); }
                                control.request_cancellation();
                                Some(advance.await)
                            },
                            () = async {
                                match deadline {
                                    Some(deadline) => tokio::time::sleep_until(deadline).await,
                                    None => std::future::pending().await,
                                }
                            } => {
                                if let Some(model) = &model { model.cancel(); }
                                None
                            },
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
                        Ok(WorkbenchRunAdvance::ParkGreen) => behavior.owned_green_task(owned),
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
                        )) => workbench
                            .bind_tool_result(
                                context,
                                frame.handle().to_owned(),
                                frame.output().to_owned(),
                            )
                            .await
                            .map(Some),
                        _ => Ok(None),
                    };
                    (answer, binding)
                })
            },
            |behavior, _kernel, mut owned, (answer, binding)| {
                let binding = binding.and_then(|binding| {
                    if let Some(binding) = binding {
                        if owned
                            .state
                            .effects
                            .control
                            .as_ref()
                            .is_some_and(|control| control.cancellation_requested())
                        {
                            return Err(ResidentActorWorkbenchError::ActorProtocol(
                                "tool-result retention cancelled before acceptance".into(),
                            ));
                        }
                        binding.accept()?;
                    }
                    Ok(())
                });
                let mut slot = owned
                    .state
                    .cursor
                    .after_tool
                    .take()
                    .expect("same after-tool invocation");
                owned.state.effects.after_tool_active = false;
                owned.state.cursor.running = None;
                // Failed cleanup remains an execution failure. Earlier completed
                // machine items may survive; this result is never an acknowledged timeout.
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
                        super::merge_retained_bindings(
                            &mut slot.receipt,
                            &owned.state.cursor.unit.recovered_bindings,
                        );
                        slot.receipt.operations =
                            std::mem::take(&mut owned.state.cursor.unit.operations);
                        owned.state.cursor.receipts.push(slot.receipt);
                        let failure = WorkbenchExecutionFailure {
                            receipts: std::mem::take(&mut owned.state.cursor.receipts),
                            point: WorkbenchFailurePoint::InputUnit {
                                index: owned.state.cursor.index,
                            },
                            publication: None,
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
                super::merge_retained_bindings(
                    &mut slot.receipt,
                    &owned.state.cursor.unit.recovered_bindings,
                );
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
        tracing::info!(actor = %frame.actor(), tool = %frame.call().name,
            ordinal = frame.ordinal(), elapsed_ms = elapsed.as_millis(),
            disposition = ?disposition, detail = %detail, "after-tool slot invoked");
        self.after_tool.record(Invocation {
            ordinal: frame.ordinal(),
            tool: frame.call().name.clone(),
            elapsed,
            provenance: frame.provenance(),
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
            OwnedWorkbenchWait::Display {
                boundary,
                allowance,
                operation,
            } => {
                let environment = self.environment.clone();
                let context = owned.state.effects.context.clone();
                return Self::owned_step_task(
                    owned,
                    move |owned| {
                        Box::pin(async move {
                            let cursor = &mut owned.state.cursor;
                            let current = cursor.running.as_mut().expect("same display fragment");
                            let receipt = DisplayReceiptSubmission {
                                owner: owned.state.effects.display_receipt_owner.clone(),
                                settlement: current
                                    .inflight_effect
                                    .as_ref()
                                    .expect("captured display effect")
                                    .display_settlement
                                    .clone(),
                                fragment: current
                                    .fragment
                                    .as_mut()
                                    .expect("display owns its fragment"),
                                remaining: &mut cursor.unit.display_remaining,
                            };
                            let prepared = prepare_actor_display_boundary(
                                &environment,
                                &context,
                                boundary,
                                allowance,
                                operation,
                                Some(receipt),
                            )
                            .await;
                            if let Ok((_, Some(display))) = &prepared {
                                let cursor = &mut owned.state.cursor;
                                let current =
                                    cursor.running.as_mut().expect("same display fragment");
                                current
                                    .inflight_effect
                                    .as_mut()
                                    .expect("captured display effect")
                                    .display = Some(display.clone());
                                let rendered = display.text.clone();
                                if !rendered.is_empty() {
                                    cursor.unit.command_output.push(rendered);
                                }
                            }
                            prepared
                        })
                    },
                    move |behavior, kernel, mut owned, prepared| match prepared {
                        Ok((boundary, display)) => {
                            let context = &owned.state.effects.context;
                            let operation = behavior
                                .prepare_independent_effect(
                                    kernel,
                                    context,
                                    &CurrentEffectOwner::Workbench(&owned.state.effects),
                                    boundary,
                                )
                                .unwrap_or_else(|_| {
                                    unreachable!(
                                        "display acknowledgement is independently resumable"
                                    )
                                });
                            let pending = ParkedWorkbenchEffect {
                                wait: OwnedWorkbenchWait::Prepared(operation),
                                display,
                                ..pending
                            };
                            Ok(WorkbenchAdvance::Park(behavior.resume_owned_effect_task(
                                owned,
                                kernel.clone(),
                                pending,
                            )))
                        }
                        Err(error) => {
                            let current = owned
                                .state
                                .cursor
                                .running
                                .as_mut()
                                .expect("same display fragment");
                            current.inflight_effect = None;
                            current.resume_failure = Some(error);
                            record_workbench_operation(
                                &mut owned.state.cursor.unit.operations,
                                owned.state.request.execution_id(),
                                owned.state.cursor.index,
                                pending.ordinal,
                                &pending.effect,
                                None,
                                pending.started.elapsed(),
                                WorkbenchOperationDisposition::Unknown,
                            );
                            Ok(WorkbenchAdvance::Park(Self::continue_owned_task(owned)))
                        }
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
                            display: pending.display,
                            wait: OwnedWorkbenchWait::Prepared(operation),
                            ordinal: pending.ordinal,
                            effect: pending.effect,
                            started: pending.started,
                            success_disposition: pending.success_disposition,
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
                .capabilities()
                .effect_keys()
                .contains(&crate::ActorEffectKey::Commands);
            return OwnedWorkbenchTask::new(Box::pin(async move {
                let (timing, cleanup) = owned.scopes();
                let control = owned
                    .state
                    .effects
                    .control
                    .clone()
                    .expect("original cancellation owner");
                let model = owned.state.effects.model.clone();
                let retirement = owned.retirement.clone();
                let deadline = owned
                    .state
                    .cursor
                    .after_tool
                    .as_ref()
                    .filter(|slot| slot.enforce_deadline)
                    .map(|slot| slot.frame.deadline());
                let cancel = owned
                    .state
                    .effects
                    .control
                    .as_ref()
                    .expect("original cancellation owner")
                    .native_cancel();
                let preparing = crate::resident_workbench::with_invocation_cancellation(
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
                );
                let operation = async {
                    tokio::pin!(preparing);
                    tokio::select! {
                        prepared = &mut preparing => prepared,
                        () = async {
                            match deadline {
                                Some(deadline) => tokio::time::sleep_until(deadline).await,
                                None => std::future::pending().await,
                            }
                        } => {
                            if let Some(model) = &model { model.cancel(); }
                            control.request_cancellation();
                            preparing.await
                        },
                    }
                };
                let prepared =
                    join_execution_step(operation, control.clone(), retirement, model.clone())
                        .await;
                OwnedWorkbenchCompletion::advance(move |behavior: &mut Self, _kernel| {
                    let (timing, cleanup) = owned.scopes();
                    timing.sync_scope(|| {
                        cleanup.sync_scope(|| {
                            let result = prepared.and_then(|prepared| {
                                if control.cancellation_requested() {
                                    return Err(ResidentActorWorkbenchError::ActorProtocol(
                                        "command presentation cancelled before acceptance".into(),
                                    ));
                                }
                                let cursor = &mut owned.state.cursor;
                                let fragment = cursor
                                    .running
                                    .as_mut()
                                    .expect("same presentation fragment")
                                    .fragment
                                    .as_mut()
                                    .expect("presentation retains its fragment");
                                prepared.apply(
                                    &environment.commands,
                                    owned.state.effects.context.actor,
                                    fragment,
                                    &mut cursor.unit.display_remaining,
                                    &mut cursor.unit.command_output,
                                    &mut cursor.unit.recovered_bindings,
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
                                    pending.display.clone(),
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
            .capabilities()
            .effect_keys()
            .contains(&crate::ActorEffectKey::Commands);
        let context = owned.state.effects.context.clone();
        let pending = {
            let ParkedWorkbenchEffect {
                display,
                success_disposition,
                wait,
                ordinal,
                effect,
                started,
            } = pending;
            let wait = match wait {
                OwnedWorkbenchWait::Command {
                    continuation,
                    request: crate::generated::commands::CommandsReq::CommandRetainJobWith(job),
                } => {
                    let workbench = owned
                        .workbench
                        .as_ref()
                        .expect("prepared execution has its workbench")
                        .shared_for_command_binding();
                    let jobs = environment.commands.clone();
                    let context = context.clone();
                    OwnedWorkbenchWait::RetainCommandBinding {
                        continuation,
                        operation: Box::pin(async move {
                            command_presentation::retain_job_binding(
                                &jobs,
                                &context,
                                &workbench,
                                job,
                                commands_permitted,
                            )
                            .await
                        }),
                    }
                }
                wait => wait,
            };
            ParkedWorkbenchEffect {
                display,
                success_disposition,
                wait,
                ordinal,
                effect,
                started,
            }
        };
        let control = owned
            .state
            .effects
            .control
            .clone()
            .unwrap_or_else(crate::WorkbenchExecutionControl::untracked);
        let invocation = owned.state.effects.invocation_work.clone();
        let model = owned.state.effects.model.clone();
        if owned.state.cursor.running.as_ref().is_some_and(|current| current.green.is_some()) {
            let (wait, receipt) = green::EffectReceipt::split(pending);
            let control = owned.state.cursor.running.as_ref().and_then(|current| current.green.as_ref())
                .and_then(green::GreenThreads::active_wait_control).unwrap_or(control);
            // Each child's expiry claim is independent. Whole-cell cancellation
            // is still observed by the original owned Green task.
            control.arm_sleep();
            let operation = Box::pin(await_effect(
                environment, kernel, context, control, wait, commands_permitted, invocation, model,
            ));
            let mut owned = owned;
            let current = owned.state.cursor.running.as_mut().expect("async original fragment");
            current.inflight_effect = None;
            current.green.as_mut().expect("async frontiers").enqueue_effect(
                std::mem::take(&mut current.scopes), receipt, operation,
            );
            return self.owned_green_task(owned);
        }
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
                    model,
                ))
            },
            move |behavior, _kernel, mut owned, mut result| {
                let retained_binding = match result.retained_job_binding.take() {
                    Some(binding)
                        if !owned
                            .state
                            .effects
                            .control
                            .as_ref()
                            .is_some_and(|control| control.cancellation_requested()) =>
                    {
                        match binding.accept() {
                            Ok(name) => Some(name),
                            Err(error) => {
                                result.disposition = WorkbenchOperationDisposition::Unknown;
                                result.outcome = Err(error);
                                None
                            }
                        }
                    }
                    _ => None,
                };
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
                    pending.display.clone(),
                    pending.started.elapsed(),
                    scoped_operation_disposition(pending.success_disposition, result.disposition),
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
                if let Some(binding) = retained_binding {
                    owned
                        .state
                        .cursor
                        .unit
                        .recovered_bindings
                        .push(binding.clone());
                    owned
                        .state
                        .cursor
                        .running
                        .as_mut()
                        .expect("retained command binding belongs to this effect fragment")
                        .fragment
                        .as_mut()
                        .expect("captured command owns its fragment")
                        .retain_job_binding(binding);
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

    fn owned_green_task(&self, owned: OwnedExecution<H, O>) -> OwnedWorkbenchTask<Self> {
        Self::owned_step_task(
            owned,
            |owned| Box::pin(async move {
                let control = owned.state.effects.control.clone().expect("original async cancellation owner");
                let green = owned.state.cursor.running.as_mut().expect("original async fragment")
                    .green.as_mut().expect("async frontiers");
                tokio::select! {
                    biased;
                    () = control.wait_for_cancellation() => {
                        green.cancel_parent();
                        Err(ResidentActorWorkbenchError::ActorProtocol("async invocation cancelled".into()))
                    }
                    ready = green.next() => ready,
                }
            }),
            |behavior, kernel, mut owned, result| {
                let applied = result.and_then(|completion| behavior.apply_green_completion(&mut owned.state, completion));
                match applied {
                    Ok(true) => Ok(WorkbenchAdvance::Park(Self::continue_owned_task(owned))),
                    Ok(false) => Ok(WorkbenchAdvance::Park(behavior.owned_green_task(owned))),
                    Err(error) => {
                        let failure = workbench_failure(
                            &owned.state.cursor.receipts, owned.state.cursor.index,
                            owned.state.request.items.len(), error,
                        );
                        Self::begin_owned_finalization(behavior, kernel, owned, Err(failure))
                    }
                }
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
        mut result: Result<KernelStep<WorkbenchResponse>, WorkbenchExecutionFailure>,
    ) -> Result<WorkbenchAdvance<Self>, KernelInvocationFailure> {
        // Error paths bypass response rendering, but publication requires the
        // same checked item kinds as a successful response.
        let receipts = match &mut result {
            Ok(KernelStep::Continue(response) | KernelStep::ContinueLater(response))
            | Ok(KernelStep::Stop {
                output: response, ..
            }) => &mut response.items,
            Err(failure) => &mut failure.receipts,
        };
        annotate_workbench_receipts(
            receipts,
            owned
                .state
                .cursor
                .cell_check
                .as_ref()
                .map(|checked| checked.items.as_slice()),
        );
        if owned.private.is_some() && private_nonpublication_reason(&result).is_none() {
            if !owned.state.effects.invocation_work.begin_publication() {
                let failure = private_publication_rejection(
                    owned.state.effects.control.as_ref().unwrap(),
                    result,
                    ResidentActorWorkbenchError::ActorProtocol(
                        "invocation closed before publication began".into(),
                    ),
                );
                return Self::settle_owned_execution(behavior, kernel, owned, Err(failure));
            }
            return Ok(WorkbenchAdvance::Park(Self::publish_owned_execution_task(
                owned,
                behavior.environment.clone(),
                kernel.clone(),
                result,
            )));
        }
        owned.state.effects.invocation_work.close();
        owned
            .state
            .effects
            .control
            .as_ref()
            .expect("owned execution retains its original control")
            .publication_decision()
            .terminate();
        if owned.private.is_some() {
            let reason = if owned
                .state
                .effects
                .control
                .as_ref()
                .unwrap()
                .native_cancel()
                .load(std::sync::atomic::Ordering::Acquire)
            {
                WorkbenchNotPublishedReason::Cancelled
            } else {
                private_nonpublication_reason(&result)
                    .expect("unsuccessful admitted cell has a nonpublication reason")
            };
            set_private_publication(
                &mut result,
                WorkbenchPublicationOutcome::NotPublished { reason },
            );
        }
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
                    Ok(PrivateExecutionPublication::Manifest {
                        commit: PublicManifestCommit::Durable | PublicManifestCommit::Ephemeral,
                        native_bindings,
                        ..
                    }) => Self::settle_owned_execution(
                        behavior,
                        &kernel,
                        owned,
                        mark_private_publication(result, &native_bindings),
                    ),
                    Ok(PrivateExecutionPublication::Manifest {
                        commit: PublicManifestCommit::PublishedDurabilityUnconfirmed { detail },
                        native_bindings,
                        ..
                    }) => Ok(WorkbenchAdvance::Park(
                        Self::confirm_owned_publication_task(
                            owned,
                            environment,
                            kernel,
                            result,
                            detail,
                            native_bindings,
                        ),
                    )),
                    Ok(PrivateExecutionPublication::Manifest {
                        commit: PublicManifestCommit::Cancelled,
                        ..
                    }) => {
                        if !owned
                            .state
                            .effects
                            .control
                            .as_ref()
                            .unwrap()
                            .native_cancel()
                            .load(std::sync::atomic::Ordering::Acquire)
                        {
                            let error = ResidentActorWorkbenchError::ActorProtocol(
                                "private publication refused after execution terminated".into(),
                            );
                            let failure = private_publication_rejection(
                                owned.state.effects.control.as_ref().unwrap(),
                                result,
                                error,
                            );
                            return Self::settle_owned_execution(
                                behavior,
                                &kernel,
                                owned,
                                Err(failure),
                            );
                        }
                        let mut result = result;
                        match &mut result {
                            Ok(
                                KernelStep::Continue(response)
                                | KernelStep::ContinueLater(response),
                            )
                            | Ok(KernelStep::Stop {
                                output: response, ..
                            }) => response.status = WorkbenchRunStatus::RequestCancelled,
                            Err(_) => {}
                        }
                        set_private_publication(
                            &mut result,
                            WorkbenchPublicationOutcome::NotPublished {
                                reason: WorkbenchNotPublishedReason::Cancelled,
                            },
                        );
                        Self::settle_owned_execution(behavior, &kernel, owned, result)
                    }
                    failure => {
                        let error = match failure {
                            Err(error) => error,
                            Ok(PrivateExecutionPublication::Rejected { reason, diagnostic }) => {
                                ResidentActorWorkbenchError::ActorProtocol(format!(
                                    "private publication rejected: {reason:?}: {diagnostic}"
                                ))
                            }
                            Ok(PrivateExecutionPublication::Manifest { commit, .. }) => {
                                ResidentActorWorkbenchError::ActorProtocol(format!(
                                    "private publication did not commit: {commit:?}"
                                ))
                            }
                        };
                        let failure = private_publication_rejection(
                            owned
                                .state
                                .effects
                                .control
                                .as_ref()
                                .expect("owned execution retains its original control"),
                            result,
                            error,
                        );
                        Self::settle_owned_execution(behavior, &kernel, owned, Err(failure))
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
        native_bindings: Vec<String>,
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
                    Ok(()) => mark_private_publication(result, &native_bindings),
                    Err(error) => {
                        let unconfirmed = private_publication_bindings(&result, &native_bindings);
                        let failure_detail = format!(
                            "published write durability remains unconfirmed: {detail}; {error}"
                        );
                        Err(private_publication_failure(
                            result,
                            ResidentActorWorkbenchError::ActorProtocol(failure_detail.clone()),
                            WorkbenchPublicationOutcome::DurabilityUnconfirmed {
                                bindings: unconfirmed,
                                detail: failure_detail,
                            },
                        ))
                    }
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
                    settle_execution_finalization(owned, environment, finalization).await
                })
            },
            |behavior, _kernel, mut owned, result| {
                let (result, notifications) =
                    behavior.complete_workbench_finalization(&mut owned.state, result);
                if !notifications.is_empty()
                    || behavior.environment.requests.has_settlement_notifications()
                {
                    let environment = behavior.environment.clone();
                    return Ok(WorkbenchAdvance::Park(Self::owned_step_task(
                        owned,
                        move |_owned| {
                            Box::pin(async move {
                                publish_request_notifications(
                                    &environment.requests,
                                    &environment.deployments,
                                    notifications,
                                )
                                .await;
                            })
                        },
                        move |_behavior, _kernel, owned, ()| {
                            Self::complete_owned_finalization(owned, result)
                        },
                    )));
                }
                Self::complete_owned_finalization(owned, result)
            },
        )
    }

    fn complete_owned_finalization(
        mut owned: OwnedExecution<H, O>,
        result: Result<KernelStep<WorkbenchResponse>, KernelInvocationFailure>,
    ) -> Result<WorkbenchAdvance<Self>, KernelInvocationFailure> {
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
    }
}

fn private_nonpublication_reason(
    result: &Result<KernelStep<WorkbenchResponse>, WorkbenchExecutionFailure>,
) -> Option<WorkbenchNotPublishedReason> {
    let response = match result {
        Ok(KernelStep::Continue(response) | KernelStep::ContinueLater(response))
        | Ok(KernelStep::Stop {
            output: response, ..
        }) => response,
        Err(_) => return Some(WorkbenchNotPublishedReason::Failed),
    };
    match response.status {
        WorkbenchRunStatus::Rejected => Some(WorkbenchNotPublishedReason::Rejected),
        WorkbenchRunStatus::RequestCancelled => Some(WorkbenchNotPublishedReason::Cancelled),
        WorkbenchRunStatus::Committed | WorkbenchRunStatus::Completed
            if response.next_index == response.total =>
        {
            None
        }
        WorkbenchRunStatus::Replied | WorkbenchRunStatus::Backgrounded => None,
        _ => Some(WorkbenchNotPublishedReason::Failed),
    }
}

fn set_private_publication(
    result: &mut Result<KernelStep<WorkbenchResponse>, WorkbenchExecutionFailure>,
    publication: WorkbenchPublicationOutcome,
) {
    match result {
        Ok(KernelStep::Continue(response) | KernelStep::ContinueLater(response))
        | Ok(KernelStep::Stop {
            output: response, ..
        }) => response.publication = Some(publication),
        Err(failure) => failure.publication = Some(publication),
    }
}

fn private_publication_bindings(
    result: &Result<KernelStep<WorkbenchResponse>, WorkbenchExecutionFailure>,
    native_bindings: &[String],
) -> Vec<String> {
    let receipts = match result {
        Ok(KernelStep::Continue(response) | KernelStep::ContinueLater(response))
        | Ok(KernelStep::Stop {
            output: response, ..
        }) => &response.items,
        Err(failure) => &failure.receipts,
    };
    receipts
        .iter()
        .flat_map(|receipt| receipt.installed_bindings.iter().cloned())
        .chain(native_bindings.iter().cloned())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn mark_private_publication(
    mut result: Result<KernelStep<WorkbenchResponse>, WorkbenchExecutionFailure>,
    native_bindings: &[String],
) -> Result<KernelStep<WorkbenchResponse>, WorkbenchExecutionFailure> {
    let bindings = private_publication_bindings(&result, native_bindings);
    let publication = WorkbenchPublicationOutcome::Published { bindings };
    set_private_publication(&mut result, publication);
    result
}

fn private_publication_rejection(
    control: &crate::WorkbenchExecutionControl,
    result: Result<KernelStep<WorkbenchResponse>, WorkbenchExecutionFailure>,
    source: ResidentActorWorkbenchError,
) -> WorkbenchExecutionFailure {
    // Settle the arbiter before reading its sticky winning-cancellation proof.
    // A claimed write that fails before rename retains its real failure.
    control.publication_decision().terminate();
    let publication = if control
        .native_cancel()
        .load(std::sync::atomic::Ordering::Acquire)
    {
        WorkbenchPublicationOutcome::NotPublished {
            reason: WorkbenchNotPublishedReason::Cancelled,
        }
    } else {
        WorkbenchPublicationOutcome::Rejected {
            detail: source.to_string(),
        }
    };
    private_publication_failure(result, source, publication)
}

fn private_publication_failure(
    result: Result<KernelStep<WorkbenchResponse>, WorkbenchExecutionFailure>,
    source: ResidentActorWorkbenchError,
    publication: WorkbenchPublicationOutcome,
) -> WorkbenchExecutionFailure {
    match result {
        Ok(KernelStep::Continue(response) | KernelStep::ContinueLater(response))
        | Ok(KernelStep::Stop {
            output: response, ..
        }) => WorkbenchExecutionFailure {
            receipts: response.items,
            point: WorkbenchFailurePoint::Publication {
                completed_input_units: response.next_index,
            },
            publication: Some(publication),
            total: response.total,
            source,
        },
        Err(failure) => WorkbenchExecutionFailure {
            source,
            publication: Some(publication),
            ..failure
        },
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
    model: Option<Arc<dyn crate::CellModelBinding>>,
) -> commands::CommandResolution
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    let result = match wait {
        OwnedWorkbenchWait::Launch(_) | OwnedWorkbenchWait::Display { .. } => {
            unreachable!("child launch requires fenced actor application")
        }
        OwnedWorkbenchWait::Prepared(operation) => operation.await,
        OwnedWorkbenchWait::RetainCommandBinding {
            continuation,
            operation,
        } => {
            let (answer, retained_job_binding) = operation.await;
            let disposition = commands::disposition(&answer);
            let outcome = environment
                .runner
                .resume_value(context.clone(), continuation, answer)
                .await;
            return commands::CommandResolution {
                disposition,
                outcome,
                started_job: None,
                retained_job_binding,
            };
        }
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
                model,
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
        retained_job_binding: None,
    }
}

pub(super) async fn await_external<H, O>(
    environment: ResidentEnvironment<H, O>,
    kernel: KernelContext,
    context: ActorSessionContext,
    control: Option<Arc<crate::WorkbenchExecutionControl>>,
    continuation: ResidentHole,
    work: tidepool_effect::DeferredEffect,
    model: Option<Arc<dyn crate::CellModelBinding>>,
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
            if let Some(model) = &model { model.cancel(); }
            if let Some(cancellation) = cancellation { cancellation.request(); }
            // Cancellation requests do not prove owner settlement.
            running.await
        }
        _ = kernel.wait_requested_shutdown() => {
            if let Some(model) = &model { model.cancel(); }
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
        fn stage_spec_reload(
            self: Arc<Self>,
            _: tidepool_repr::PrincipalId,
            _: &[String],
        ) -> Result<Box<dyn crate::StagedActorSourceReload>, crate::SourceLayerReload> {
            Err(crate::SourceLayerReload::Unavailable(
                "this source fixture installs no agent spec".into(),
            ))
        }

        fn validate_source_authority(
            &self,
            source: &crate::CheckpointSourceLayer,
        ) -> Result<(), String> {
            self.0
                .owns(source)
                .then_some(())
                .ok_or_else(|| "foreign source issuer".into())
        }

        fn layer_include_for(&self, _: &str) -> Result<Vec<PathBuf>, String> {
            Ok(Vec::new())
        }

        fn bind_for(&self, _: tidepool_repr::PrincipalId, _: &str) {}
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
    fn effect_profile_is_part_of_exact_compilation_authority() {
        let source = crate::CheckpointSourceLayer::default();
        let mut async_context = context();
        async_context.haskell_effects_alias = "'[Replies]".into();
        async_context.source_layer = Arc::from([]);
        let mut sync_context = async_context.clone();
        sync_context.haskell_effects_alias = "'[ContextReadWrite, Replies]".into();
        let (_, asynchronous) =
            WorkbenchCompilationAuthority::admit(async_context, source.clone(), None, None)
                .unwrap();
        let (_, synchronous) =
            WorkbenchCompilationAuthority::admit(sync_context, source, None, None).unwrap();
        assert_ne!(
            asynchronous.authority_digest(),
            synchronous.authority_digest()
        );
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
