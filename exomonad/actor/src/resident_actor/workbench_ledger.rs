use super::*;

#[cfg(test)]
async fn settle_provider_child(
    kernel: &KernelContext,
    child: ActorRef,
) -> Result<(), KernelBehaviorError> {
    let child_owner = kernel.resolve(child).ok_or_else(|| {
        KernelBehaviorError::new(format!(
            "provider child {child:?} lacks retained cleanup ownership"
        ))
    })?;
    settle_provider_child_owner(child_owner, provider_child_terminal()).await
}

fn provider_child_terminal() -> ActorTerminal {
    ActorTerminal {
        kind: ActorExitKind::Cancelled,
        summary: "enclosing provider boundary released incomplete child".into(),
        diagnostic: None,
    }
}

pub(super) async fn settle_provider_child_owner(
    child_owner: LocalActorRef,
    terminal: ActorTerminal,
) -> Result<(), KernelBehaviorError> {
    let child = child_owner.identity();
    let shutdown = child_owner
        .shutdown_with_cleanup(terminal)
        .await
        .map_err(KernelInvocationFailure::into_behavior_error)?;
    if !shutdown.cleanup.is_confirmed() {
        return Err(KernelBehaviorError::new(format!(
            "provider child {child:?} cleanup is unconfirmed"
        )));
    }
    Ok(())
}

pub(super) async fn settle_provider_children(
    journal: &Arc<Mutex<WorkbenchExecutions>>,
    kernel: &KernelContext,
    boundary: &tidepool_runtime::session::ContextCheckpointBoundary,
) -> Result<(), KernelBehaviorError> {
    let children = journal.lock().provider_children(boundary)?;
    let batch = provider_child_retirement_batch(kernel, children)?;
    for (owner, terminal) in batch.into_actors() {
        let child = owner.identity();
        settle_provider_child_owner(owner, terminal).await?;
        journal.lock().provider_child_released(boundary, child)?;
    }
    Ok(())
}

pub(super) fn provider_child_retirement_batch(
    kernel: &KernelContext,
    children: Vec<ActorRef>,
) -> Result<crate::kernel::RetirementBatch, KernelBehaviorError> {
    let owners = children
        .into_iter()
        .map(|child| {
            kernel
                .resolve(child)
                .map(|owner| (owner, provider_child_terminal()))
                .ok_or_else(|| {
                    KernelBehaviorError::new(format!(
                        "provider child {child:?} lacks retained cleanup ownership"
                    ))
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(crate::kernel::RetirementBatch::issue(owners))
}

#[derive(Clone)]
struct WorkbenchExecutionRecord {
    request: WorkbenchRequest,
    state: WorkbenchExecutionState,
    invocation_work: Option<Arc<InvocationWork>>,
    cell_terminal: Option<crate::CellExit>,
    boundary_abort: Option<BoundaryAbortCleanup>,
    display_settlements: Arc<DisplayExecutionSettlement>,
    provider_finalization: Option<Arc<crate::resident_tools::ProviderFinalization>>,
    provider_owner: Option<crate::HostedOperationSettlement>,
    provider_children: Vec<ActorRef>,
}

#[derive(Clone, Default)]
pub(super) struct BoundaryAbortCleanup {
    pub children: Vec<ActorRef>,
    pub scopes: Vec<tidepool_codegen::scope::ScopeId>,
}

/// Exact admitted journal owner for abort extraction and progressive cleanup.
pub(super) struct BoundaryAbortOwner {
    journal: Arc<Mutex<WorkbenchExecutions>>,
    key: WorkbenchReplayKey,
}

impl BoundaryAbortOwner {
    pub(super) fn collect_cleanup(
        &self,
        collect: impl FnOnce() -> BoundaryAbortCleanup,
    ) -> BoundaryAbortCleanup {
        let retained = self.journal.lock().0[&self.key].boundary_abort.clone();
        // The serial actor owns extraction; release the journal lock before
        // entering the checkpoint registry and retain the obligation before awaiting.
        let cleanup = retained.unwrap_or_else(collect);
        self.retain_cleanup(cleanup.clone());
        cleanup
    }

    pub(super) fn retain_cleanup(&self, cleanup: BoundaryAbortCleanup) {
        self.journal
            .lock()
            .0
            .get_mut(&self.key)
            .expect("admitted abort records remain in the journal")
            .boundary_abort = Some(cleanup);
    }
}

#[derive(Clone)]
enum WorkbenchExecutionState {
    Unconfirmed,
    Terminal {
        reply: crate::KernelWorkbenchReply,
        cancellation: crate::WorkbenchCancellationOutcome,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum WorkbenchReplayKey {
    Hosted(crate::resident_tools::WorkbenchCallKey),
    Execution(WorkbenchExecutionId),
}

impl WorkbenchReplayKey {
    fn new(
        execution: &WorkbenchExecutionId,
        invocation: Option<&crate::resident_tools::WorkbenchCallKey>,
    ) -> Self {
        invocation.map_or_else(
            || Self::Execution(execution.clone()),
            |key| Self::Hosted(key.clone()),
        )
    }
}

#[derive(Default)]
pub(super) struct WorkbenchExecutions(
    std::collections::HashMap<WorkbenchReplayKey, WorkbenchExecutionRecord>,
    // Actor callbacks have no notebook invocation, but use the same retained
    // cleanup owner and journal retirement path as admitted notebook calls.
    Vec<Arc<InvocationWork>>,
);

#[derive(Debug, PartialEq, Eq)]
pub(super) enum WorkbenchReplayFailure {
    DifferentInput,
    Unconfirmed,
}

pub(super) enum WorkbenchBoundaryRecord {
    Unconfirmed,
    Terminal(crate::KernelWorkbenchReply),
}

impl WorkbenchExecutions {
    pub(super) fn display_receipt_owner(
        &self,
        execution: &WorkbenchExecutionId,
        invocation: Option<&crate::resident_tools::WorkbenchCallKey>,
    ) -> Option<Arc<DisplayExecutionSettlement>> {
        self.0
            .get(&WorkbenchReplayKey::new(execution, invocation))
            .map(|record| record.display_settlements.clone())
    }

    pub(super) fn freeze_display_receipts(
        &self,
        execution: &WorkbenchExecutionId,
        invocation: Option<&crate::resident_tools::WorkbenchCallKey>,
        result: &mut Result<KernelStep<WorkbenchResponse>, KernelInvocationFailure>,
    ) {
        if let Some(owner) = self.display_receipt_owner(execution, invocation) {
            owner.freeze_step(result);
        }
    }

    pub(super) fn display_observations(
        &self,
    ) -> Vec<(
        WorkbenchOperationId,
        tidepool_runtime::session::WorkbenchDisplayPublication,
    )> {
        let mut observed = self
            .0
            .values()
            .flat_map(|record| record.display_settlements.observations())
            .collect::<Vec<_>>();
        observed.sort_by_key(|(id, _)| {
            (
                id.execution.to_string(),
                id.input_unit_index,
                id.effect_ordinal,
            )
        });
        observed
    }
    pub(super) fn admitted_invocation_work(
        &self,
        execution: &WorkbenchExecutionId,
        invocation: Option<&crate::resident_tools::WorkbenchCallKey>,
    ) -> Option<Arc<InvocationWork>> {
        self.0
            .get(&WorkbenchReplayKey::new(execution, invocation))?
            .invocation_work
            .clone()
    }

    pub(super) fn retain_invocation_work(
        &mut self,
        work: Arc<InvocationWork>,
        execution: Option<&WorkbenchExecutionId>,
        invocation: Option<&crate::resident_tools::WorkbenchCallKey>,
    ) {
        let execution = execution.expect("every admitted invocation has an execution identity");
        let record = self
            .0
            .get_mut(&WorkbenchReplayKey::new(execution, invocation))
            .expect("invocation follows its original journal fence");
        assert!(
            record.invocation_work.is_none(),
            "invocation membership is admitted once"
        );
        record.invocation_work = Some(work);
    }

    pub(super) fn cleanup_warnings(&self) -> Vec<String> {
        self.0
            .values()
            .filter_map(|record| {
                let work = record.invocation_work.as_ref()?;
                if !work.is_closed() {
                    return None;
                }
                let uncertainty = match work.cleanup_observation() {
                    Some(cleanup) => cleanup.uncertainty()?,
                    None => {
                        "cleanup has not been observed; cancellation remains unconfirmed".into()
                    }
                };
                Some(format!(
                    "  {}: {uncertainty}",
                    record.request.execution_id()?
                ))
            })
            .collect()
    }

    pub(super) fn invocation_work(&self) -> Vec<Arc<InvocationWork>> {
        self.0
            .values()
            .filter_map(|record| record.invocation_work.clone())
            .chain(self.1.iter().cloned())
            .collect()
    }

    pub(super) fn actor_scope_root(&mut self, actor: ActorRef) -> Arc<InvocationWork> {
        self.callback_root(actor, RequestReservationOwner::Scope(0))
    }

    pub(super) fn callback_root(
        &mut self,
        actor: ActorRef,
        reservation: RequestReservationOwner,
    ) -> Arc<InvocationWork> {
        if let Some(root) = self.1.iter().find(|root| root.matches(actor, &reservation)) {
            return root.clone();
        }
        let root = InvocationWork::new(actor, reservation);
        self.1.push(root.clone());
        root
    }

    pub(super) fn lookup(
        &self,
        execution: &WorkbenchExecutionId,
        request: &WorkbenchRequest,
        invocation: Option<&crate::resident_tools::WorkbenchCallKey>,
    ) -> Result<Option<crate::KernelWorkbenchReply>, WorkbenchReplayFailure> {
        let Some(record) = self.0.get(&WorkbenchReplayKey::new(execution, invocation)) else {
            return Ok(None);
        };
        #[allow(
            clippy::expect_used,
            reason = "every record enters this journal through `begin`, whose \
                      only caller stores it precisely when `request.execution_id()` \
                      was already Some; no path inserts a record without one"
        )]
        let comparable = request.clone().with_execution_id(
            record
                .request
                .execution_id()
                .expect("retained execution identity")
                .clone(),
        );
        if record.request != comparable {
            return Err(WorkbenchReplayFailure::DifferentInput);
        }
        match &record.state {
            WorkbenchExecutionState::Unconfirmed => Err(WorkbenchReplayFailure::Unconfirmed),
            WorkbenchExecutionState::Terminal { reply, .. } => Ok(Some(reply.clone())),
        }
    }

    pub(super) fn lookup_for_provider_control(
        &self,
        execution: &WorkbenchExecutionId,
        request: &WorkbenchRequest,
        invocation: Option<&crate::resident_tools::WorkbenchCallKey>,
        control: Option<&Arc<crate::WorkbenchExecutionControl>>,
    ) -> Result<Option<crate::KernelWorkbenchReply>, WorkbenchReplayFailure> {
        let reply = self.lookup(execution, request, invocation)?;
        if reply.is_some() {
            self.adopt_provider_replay(execution, invocation, control);
        }
        Ok(reply)
    }

    pub(super) fn begin(
        &mut self,
        execution: &WorkbenchExecutionId,
        request: WorkbenchRequest,
        invocation: Option<&crate::resident_tools::WorkbenchCallKey>,
    ) {
        self.0.insert(
            WorkbenchReplayKey::new(execution, invocation),
            WorkbenchExecutionRecord {
                request,
                state: WorkbenchExecutionState::Unconfirmed,
                invocation_work: None,
                cell_terminal: None,
                boundary_abort: None,
                display_settlements: Arc::new(DisplayExecutionSettlement::new(execution.clone())),
                provider_finalization: None,
                provider_owner: None,
                provider_children: Vec::new(),
            },
        );
    }

    pub(super) fn record(
        &mut self,
        execution: WorkbenchExecutionId,
        request: WorkbenchRequest,
        reply: crate::KernelWorkbenchReply,
        cancellation: crate::WorkbenchCancellationOutcome,
        invocation: Option<&crate::resident_tools::WorkbenchCallKey>,
    ) {
        let key = WorkbenchReplayKey::new(&execution, invocation);
        let invocation_work = self
            .0
            .get(&key)
            .and_then(|record| record.invocation_work.clone());
        let boundary_abort = self
            .0
            .get(&key)
            .and_then(|record| record.boundary_abort.clone());
        let cell_terminal = self
            .0
            .get(&key)
            .and_then(|record| record.cell_terminal.clone());
        let display_settlements = self
            .0
            .get(&key)
            .map(|record| record.display_settlements.clone())
            .unwrap_or_else(|| Arc::new(DisplayExecutionSettlement::new(execution.clone())));
        let provider_children = self
            .0
            .get(&key)
            .map(|record| record.provider_children.clone())
            .unwrap_or_default();
        let provider_owner = self
            .0
            .get(&key)
            .and_then(|record| record.provider_owner.clone());
        let provider_finalization = self
            .0
            .get(&key)
            .and_then(|record| record.provider_finalization.clone());
        self.0.insert(
            key,
            WorkbenchExecutionRecord {
                request,
                state: WorkbenchExecutionState::Terminal {
                    reply,
                    cancellation,
                },
                invocation_work,
                cell_terminal,
                boundary_abort,
                display_settlements,
                provider_finalization,
                provider_owner,
                provider_children,
            },
        );
    }

    pub(super) fn bind_provider_finalization(
        &mut self,
        execution: &WorkbenchExecutionId,
        invocation: Option<&crate::resident_tools::WorkbenchCallKey>,
        control: Option<&Arc<crate::WorkbenchExecutionControl>>,
    ) {
        let Some(control) = control else {
            return;
        };
        // Generic serial tools have actor-local controls but no provider
        // workbench custody. Their existing completion contract is unchanged.
        let Some(owner) = control
            .provider_owner
            .get()
            .and_then(|owner| owner.upgrade())
        else {
            return;
        };
        control.provider_finalization.admit();
        let record = self
            .0
            .get_mut(&WorkbenchReplayKey::new(execution, invocation))
            .expect("provider finalization follows native admission");
        record.provider_finalization = Some(control.provider_finalization.clone());
        record.provider_owner = Some(owner);
    }

    /// Called only after this journal's exact-input lookup returned a terminal
    /// reply. Physical redispatch shares the original owner, not a new outcome.
    fn adopt_provider_replay(
        &self,
        execution: &WorkbenchExecutionId,
        invocation: Option<&crate::resident_tools::WorkbenchCallKey>,
        control: Option<&Arc<crate::WorkbenchExecutionControl>>,
    ) {
        if let (Some(record), Some(control)) = (
            self.0.get(&WorkbenchReplayKey::new(execution, invocation)),
            control,
        ) {
            if let Some(owner) = &record.provider_owner {
                if control.invocation.as_ref() == Some(owner.key()) {
                    let _ = control.provider_replay.set(owner.clone());
                }
            }
        }
    }

    fn provider_cleanup_record(
        &mut self,
        boundary: &tidepool_runtime::session::ContextCheckpointBoundary,
    ) -> Result<&mut WorkbenchExecutionRecord, KernelBehaviorError> {
        let keys = self
            .0
            .iter()
            .filter(|(_, record)| record.request.checkpoint_boundary() == Some(boundary))
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        let exact = match keys.as_slice() {
            [WorkbenchReplayKey::Hosted(invocation)] => invocation.is_original_invocation(),
            [WorkbenchReplayKey::Execution(_)] => boundary.hosted().is_none(),
            _ => false,
        };
        if !exact {
            return Err(KernelBehaviorError::new(
                "provider child cleanup lacks one exact original journal owner",
            ));
        }
        Ok(self
            .0
            .get_mut(&keys[0])
            .expect("retained original journal owner"))
    }

    pub(super) fn provider_invocation_work(
        &mut self,
        boundary: &tidepool_runtime::session::ContextCheckpointBoundary,
    ) -> Result<Option<Arc<InvocationWork>>, KernelBehaviorError> {
        Ok(self
            .provider_cleanup_record(boundary)?
            .invocation_work
            .clone())
    }

    #[cfg(test)]
    pub(super) fn retain_provider_children(
        &mut self,
        boundary: &tidepool_runtime::session::ContextCheckpointBoundary,
        children: Vec<ActorRef>,
    ) -> Result<(), KernelBehaviorError> {
        if children.is_empty() {
            return Ok(());
        }
        let record = self.provider_cleanup_record(boundary)?;
        for child in children {
            if !record.provider_children.contains(&child) {
                record.provider_children.push(child);
            }
        }
        Ok(())
    }

    pub(super) fn provider_children(
        &mut self,
        boundary: &tidepool_runtime::session::ContextCheckpointBoundary,
    ) -> Result<Vec<ActorRef>, KernelBehaviorError> {
        Ok(self
            .provider_cleanup_record(boundary)?
            .provider_children
            .clone())
    }

    pub(super) fn provider_child_released(
        &mut self,
        boundary: &tidepool_runtime::session::ContextCheckpointBoundary,
        child: ActorRef,
    ) -> Result<(), KernelBehaviorError> {
        self.provider_cleanup_record(boundary)?
            .provider_children
            .retain(|pending| *pending != child);
        Ok(())
    }

    pub(super) fn finalize_provider_boundary(
        &self,
        boundary: &tidepool_runtime::session::ContextCheckpointBoundary,
        result: Result<crate::ProviderFinalizationKind, String>,
    ) {
        let records = self
            .0
            .iter()
            .filter(|(key, _)| {
                matches!(key,
            WorkbenchReplayKey::Hosted(invocation) if invocation.matches_boundary(boundary))
            })
            .collect::<Vec<_>>();
        let exact = matches!(records.as_slice(), [(WorkbenchReplayKey::Hosted(invocation), _)] if invocation.is_original_invocation());
        for (_, record) in records {
            if let Some(owner) = &record.provider_finalization {
                owner.finish(if !exact {
                    Err("nested or multiple native cells do not prove the original provider boundary".into())
                } else if result.is_ok() && !record.provider_children.is_empty() {
                    Err("provider child cleanup remains unconfirmed".into())
                } else { result.clone() });
            }
        }
    }

    pub(super) fn pending_provider_boundaries(
        &self,
    ) -> Vec<tidepool_runtime::session::ContextCheckpointBoundary> {
        let mut boundaries = Vec::new();
        for record in self.0.values().filter(|record| {
            record
                .provider_finalization
                .as_ref()
                .is_some_and(|owner| !owner.successfully_settled())
        }) {
            if let Some(boundary) = record.request.checkpoint_boundary() {
                if !boundaries.contains(boundary) {
                    boundaries.push(boundary.clone());
                }
            }
        }
        boundaries
    }

    pub(super) fn boundary_abort_owner(
        journal: &Arc<Mutex<Self>>,
        boundary: &tidepool_runtime::session::ContextCheckpointBoundary,
        has_pending_work: impl FnOnce() -> bool,
    ) -> Result<Option<BoundaryAbortOwner>, KernelBehaviorError> {
        let key = boundary
            .hosted()
            .map(|operation| {
                WorkbenchReplayKey::Hosted(crate::resident_tools::WorkbenchCallKey::original(
                    operation,
                ))
            })
            .filter(|key| journal.lock().0.contains_key(key));
        if let Some(key) = key {
            return Ok(Some(BoundaryAbortOwner {
                journal: journal.clone(),
                key,
            }));
        }
        if has_pending_work() {
            return Err(KernelBehaviorError {
                detail: "output abort has no exact admitted invocation owner".into(),
                diagnostic: None,
            });
        }
        Ok(None)
    }

    pub(super) fn retain_cell_terminal(
        &mut self,
        execution: &WorkbenchExecutionId,
        invocation: Option<&crate::resident_tools::WorkbenchCallKey>,
        exit: crate::CellExit,
    ) {
        let record = self
            .0
            .get_mut(&WorkbenchReplayKey::new(execution, invocation))
            .expect("cell terminal follows admitted execution");
        record.cell_terminal = Some(exit);
    }

    pub(super) fn cell_allows_publication(
        &self,
        boundary: &tidepool_runtime::session::ContextCheckpointBoundary,
    ) -> bool {
        self.0
            .iter()
            .filter(|(key, _)| {
                matches!(key,
            WorkbenchReplayKey::Hosted(invocation) if invocation.matches_boundary(boundary))
            })
            .all(|(_, record)| {
                record
                    .cell_terminal
                    .as_ref()
                    .is_some_and(crate::CellExit::permits_context_commit)
            })
    }

    pub(super) fn cancellation(
        &self,
        execution: WorkbenchExecutionId,
        invocation: Option<&crate::resident_tools::WorkbenchCallKey>,
    ) -> crate::WorkbenchCancellationOutcome {
        match self.0.get(&WorkbenchReplayKey::new(&execution, invocation)) {
            None => crate::WorkbenchCancellationOutcome::UnknownEvaluation { execution },
            Some(record) => match &record.state {
                WorkbenchExecutionState::Unconfirmed =>
                {
                    #[allow(
                        clippy::expect_used,
                        reason = "every record enters this journal through `begin`, whose \
                                  only caller stores it precisely when `request.execution_id()` \
                                  was already Some; no path inserts a record without one"
                    )]
                    crate::WorkbenchCancellationOutcome::Unconfirmed {
                        execution: record
                            .request
                            .execution_id()
                            .expect("retained execution identity")
                            .clone(),
                    }
                }
                WorkbenchExecutionState::Terminal { cancellation, .. } => cancellation.clone(),
            },
        }
    }

    pub(super) fn at_boundary(
        &self,
        boundary: &tidepool_runtime::session::ContextCheckpointBoundary,
    ) -> Option<WorkbenchBoundaryRecord> {
        let mut terminal = None;
        for (key, record) in &self.0 {
            let WorkbenchReplayKey::Hosted(invocation) = key else {
                continue;
            };
            if !invocation.matches_boundary(boundary) {
                continue;
            }
            // Even one completed nested call does not prove that the enclosing
            // provider program finished or what result it returned.
            if !invocation.is_original_invocation() {
                return Some(WorkbenchBoundaryRecord::Unconfirmed);
            }
            match &record.state {
                WorkbenchExecutionState::Unconfirmed => {
                    return Some(WorkbenchBoundaryRecord::Unconfirmed);
                }
                WorkbenchExecutionState::Terminal { reply, .. } => {
                    // Several local invocations may share an original model call.
                    // None of their individual replies proves its combined result.
                    if terminal.is_some() {
                        return Some(WorkbenchBoundaryRecord::Unconfirmed);
                    }
                    terminal = Some(reply.clone());
                }
            }
        }
        terminal.map(WorkbenchBoundaryRecord::Terminal)
    }

    /// Every retained execution whose outcome is known, paired with the raw
    /// request it replays (cell source included, when it was a cell
    /// submission rather than a hosted tool call) and the reply it produced.
    /// The read side of `begin`/`record`; this is the same journal that
    /// already exists for hosted-call replay dedup, not a second one kept
    /// for status rendering. Feeds `ResidentKernelBehavior::live_status_text`
    /// via `status_rendering::render_bindings_section`.
    pub(super) fn terminal_entries(
        &self,
    ) -> Vec<(
        WorkbenchExecutionId,
        &WorkbenchRequest,
        &crate::KernelWorkbenchReply,
    )> {
        self.0
            .values()
            .filter_map(|record| match &record.state {
                WorkbenchExecutionState::Terminal { reply, .. } => record
                    .request
                    .execution_id()
                    .map(|execution| (execution.clone(), &record.request, reply)),
                WorkbenchExecutionState::Unconfirmed => None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use futures_util::future::BoxFuture;
    use serde_json::Value;
    struct CleanupProbe {
        context: Arc<std::sync::OnceLock<KernelContext>>,
        fail_cleanup: bool,
    }
    impl KernelBehavior for CleanupProbe {
        fn start<'a>(
            &'a mut self,
            context: &'a KernelContext,
        ) -> BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>> {
            let _ = self.context.set(context.clone());
            Box::pin(async { Ok(KernelStep::Continue(())) })
        }
        fn cast<'a>(
            &'a mut self,
            _: &'a KernelContext,
            _: ActorRef,
            _: MailboxValue,
        ) -> BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>> {
            panic!("no mailbox effects admitted")
        }
        fn call<'a>(
            &'a mut self,
            _: &'a KernelContext,
            _: ActorRef,
            _: crate::CallAncestry,
            _: MailboxValue,
        ) -> BoxFuture<'a, Result<KernelStep<MailboxValue>, KernelBehaviorError>> {
            panic!("no mailbox effects admitted")
        }
        fn tool<'a>(
            &'a mut self,
            _: &'a KernelContext,
            _: exomonad_tool::ToolInvocation,
            _: Option<Arc<dyn crate::HostedCheckpointCapture>>,
        ) -> BoxFuture<'a, Result<KernelStep<Value>, KernelInvocationFailure>> {
            panic!("no native tool admitted")
        }
        fn workbench<'a>(
            &'a mut self,
            _: &'a KernelContext,
            _: crate::ActorWorkbenchInvocation,
            _: Option<Arc<crate::WorkbenchExecutionControl>>,
        ) -> BoxFuture<
            'a,
            Result<
                KernelStep<tidepool_runtime::session::WorkbenchResponse>,
                KernelInvocationFailure,
            >,
        > {
            panic!("no native workbench admitted")
        }
        fn external_application_failed<'a>(
            &'a mut self,
            _: &'a KernelContext,
            _: crate::ExternalApplicationFailure,
        ) -> BoxFuture<'a, crate::ExternalFailureDisposition> {
            panic!("no external application admitted")
        }
        fn shutdown<'a>(
            &'a mut self,
            _: &'a KernelContext,
            _: &'a ActorTerminal,
        ) -> BoxFuture<'a, Result<(), KernelBehaviorError>> {
            Box::pin(async { Ok(()) })
        }
        fn shutdown_components<'a>(
            &'a mut self,
            _: &'a KernelContext,
            _: &'a ActorTerminal,
            _: tokio::time::Instant,
        ) -> BoxFuture<
            'a,
            (
                crate::CleanupComponentOutcome,
                crate::CleanupComponentOutcome,
            ),
        > {
            let fail = self.fail_cleanup;
            Box::pin(async move {
                (
                    crate::CleanupComponentOutcome::Confirmed,
                    if fail {
                        crate::CleanupComponentOutcome::Unconfirmed(
                            "injected child cleanup failure".into(),
                        )
                    } else {
                        crate::CleanupComponentOutcome::Confirmed
                    },
                )
            })
        }
        fn stopped<'a>(
            &'a mut self,
            _: &'a KernelContext,
            _: &'a ActorTerminal,
        ) -> BoxFuture<'a, ()> {
            Box::pin(async {})
        }
        fn child_exited(&mut self, _: crate::ChildExitNotice) {}
    }

    #[tokio::test]
    async fn real_child_cleanup_preserves_partial_obligations_and_refuses_false_completion() {
        let context = Arc::new(std::sync::OnceLock::new());
        let (parent, task) = crate::spawn_local_actor(
            None,
            CleanupProbe {
                context: context.clone(),
                fail_cleanup: false,
            },
        )
        .await
        .unwrap();
        let kernel = context.get().unwrap().clone();
        let good = kernel
            .spawn_child(
                None,
                CleanupProbe {
                    context: Arc::default(),
                    fail_cleanup: false,
                },
            )
            .await
            .unwrap();
        let bad = kernel
            .spawn_child(
                None,
                CleanupProbe {
                    context: Arc::default(),
                    fail_cleanup: true,
                },
            )
            .await
            .unwrap();
        let invocation = exomonad_tool::ToolInvocationContext::external(
            "thread".into(),
            "turn".into(),
            "call".into(),
            Some("call".into()),
            None,
        );
        let control = crate::WorkbenchExecutionControl::from_invocation(Some(invocation));
        let key = control.invocation.as_ref().unwrap();
        let execution = control.execution_id(parent.identity());
        let boundary = tidepool_runtime::session::ContextCheckpointBoundary::external(
            "thread".into(),
            "turn".into(),
            "call".into(),
        );
        let request = WorkbenchRequest::from_cell_input("current operation")
            .with_execution_id(execution.clone())
            .with_checkpoint_boundary(boundary.clone());
        let publications = crate::kernel::HostedCellPublications::default();
        publications.publish_provider_transport(parent.identity(), control.clone());
        publications.accept(&control);
        let journal = Arc::new(Mutex::new(WorkbenchExecutions::default()));
        journal.lock().begin(&execution, request, Some(key));
        journal
            .lock()
            .bind_provider_finalization(&execution, Some(key), Some(&control));
        journal
            .lock()
            .retain_provider_children(&boundary, vec![good.identity(), bad.identity()])
            .unwrap();
        let result = settle_provider_children(&journal, &kernel, &boundary).await;
        assert!(result.is_err());
        assert!(good.terminal().cleanup().unwrap().is_confirmed());
        assert!(!bad.terminal().cleanup().unwrap().is_confirmed());
        assert_eq!(
            journal.lock().provider_children(&boundary).unwrap(),
            vec![bad.identity()]
        );
        assert!(
            settle_provider_children(&journal, &kernel, &boundary)
                .await
                .is_err(),
            "repeat uses the same unconfirmed child owner"
        );
        assert_eq!(
            journal.lock().provider_children(&boundary).unwrap(),
            vec![bad.identity()]
        );
        assert!(kernel.forget_terminal_actor(bad.identity()));
        assert!(kernel.resolve(bad.identity()).is_none());
        assert!(settle_provider_child(&kernel, bad.identity())
            .await
            .is_err());
        assert!(settle_provider_children(&journal, &kernel, &boundary)
            .await
            .is_err());
        assert_eq!(
            journal.lock().provider_children(&boundary).unwrap(),
            vec![bad.identity()]
        );
        let abort_owner = WorkbenchExecutions::boundary_abort_owner(&journal, &boundary, || false)
            .unwrap()
            .unwrap();
        let abort_cleanup = abort_owner.collect_cleanup(|| BoundaryAbortCleanup {
            children: vec![bad.identity()],
            scopes: Vec::new(),
        });
        assert!(settle_provider_child(&kernel, abort_cleanup.children[0])
            .await
            .is_err());
        assert_eq!(
            abort_owner
                .collect_cleanup(|| panic!("lost child remains owned"))
                .children,
            vec![bad.identity()]
        );
        // The finalization owner itself prevents a caller from asserting
        // Completed while its actual child cleanup obligation remains retained.
        journal
            .lock()
            .finalize_provider_boundary(&boundary, Ok(crate::ProviderFinalizationKind::Completed));
        assert!(matches!(
            publications
                .retained_boundary(&boundary)
                .unwrap()
                .finalization(),
            crate::HostedOperationFinalization::Settled(Err(_))
        ));
        assert!(!journal.lock().pending_provider_boundaries().is_empty());
        let other = tidepool_runtime::session::ContextCheckpointBoundary::external(
            "thread".into(),
            "turn".into(),
            "other".into(),
        );
        assert!(settle_provider_children(&journal, &kernel, &other)
            .await
            .is_err());
        parent
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Cancelled,
                summary: "cleanup history complete".into(),
                diagnostic: None,
            })
            .await
            .unwrap();
        task.await.unwrap();
    }

    #[test]
    fn generic_tool_control_does_not_acquire_workbench_provider_custody() {
        let actor = crate::ActorRef::first(crate::ActorId(7));
        let invocation = exomonad_tool::ToolInvocationContext::external(
            "thread".into(),
            "turn".into(),
            "generic".into(),
            None,
            None,
        );
        let control = crate::WorkbenchExecutionControl::from_invocation(Some(invocation));
        let key = control.invocation.as_ref().unwrap();
        let execution = control.execution_id(actor);
        let request = WorkbenchRequest::from_cell_input("genericHandler")
            .with_execution_id(execution.clone());
        let slot = crate::kernel::HostedCellPublications::default();
        slot.claim(&control);
        let mut journal = WorkbenchExecutions::default();
        journal.begin(&execution, request, Some(key));
        journal.bind_provider_finalization(&execution, Some(key), Some(&control));
        assert!(journal.pending_provider_boundaries().is_empty());
        assert!(control.provider_owner.get().is_none());
        slot.complete(&control);
        assert!(slot.retained_operation(key).unwrap().is_none());
        assert!(slot.find(|_| true).is_none());
    }

    #[tokio::test]
    async fn exact_native_replay_shares_provider_settlement_after_retirement() {
        use crate::{
            HostedOperationFinalization, HostedOperationTerminal, ProviderFinalizationKind,
        };
        let actor = crate::ActorRef::first(crate::ActorId(7));
        let invocation = exomonad_tool::ToolInvocationContext::external(
            "thread".into(),
            "turn".into(),
            "call".into(),
            Some("call".into()),
            None,
        );
        let control = crate::WorkbenchExecutionControl::from_invocation(Some(invocation.clone()));
        let key = control.invocation.as_ref().unwrap();
        let execution = control.execution_id(actor);
        let boundary = tidepool_runtime::session::ContextCheckpointBoundary::external(
            "thread".into(),
            "turn".into(),
            "call".into(),
        );
        let request = WorkbenchRequest::from_cell_input("respond value")
            .with_execution_id(execution.clone())
            .with_checkpoint_boundary(boundary.clone());
        let slot = crate::kernel::HostedCellPublications::default();
        slot.publish_provider_transport(actor, control.clone());
        slot.accept(&control);
        let mut journal = WorkbenchExecutions::default();
        journal.begin(&execution, request.clone(), Some(key));
        journal.bind_provider_finalization(&execution, Some(key), Some(&control));
        let pending_retry =
            crate::WorkbenchExecutionControl::from_invocation(Some(invocation.clone()));
        slot.publish_provider_transport(actor, pending_retry.clone());
        slot.accept(&pending_retry);
        assert_eq!(
            journal.lookup_for_provider_control(
                &execution,
                &request,
                Some(key),
                Some(&pending_retry)
            ),
            Err(WorkbenchReplayFailure::Unconfirmed)
        );
        pending_retry.settle_not_admitted(Err(KernelInvocationFailure::Rejected {
            actor,
            receipts: Vec::new(),
            detail: "original pending".into(),
            diagnostic: None,
        }));
        slot.complete(&pending_retry);
        let pending_original = slot.retained_boundary(&boundary).unwrap();
        assert!(matches!(
            pending_original.terminal(),
            HostedOperationTerminal::Pending
        ));
        assert!(matches!(
            pending_original.finalization(),
            HostedOperationFinalization::Pending
        ));
        let reply = Ok(WorkbenchResponse {
            status: WorkbenchRunStatus::Completed,
            summary: None,
            items: Vec::new(),
            next_index: 0,
            total: 0,
            publication: None,
        });
        control.request_cancellation();
        control.finish_cell(
            execution.clone(),
            &Ok(KernelStep::Continue(reply.clone().unwrap())),
            true,
        );
        control.settle(reply.clone());
        slot.complete(&control);
        let cancellation = control.cancellation_outcome(execution.clone(), reply.clone());
        journal.record(
            execution.clone(),
            request.clone(),
            reply.clone(),
            cancellation,
            Some(key),
        );
        journal
            .finalize_provider_boundary(&boundary, Ok(ProviderFinalizationKind::RetirementAborted));
        let original = slot.retained_boundary(&boundary).unwrap();
        assert!(matches!(
            original.terminal(),
            HostedOperationTerminal::Settled(crate::WorkbenchCancellationOutcome::Cancelled { .. })
        ));
        assert_eq!(
            original.finalization(),
            HostedOperationFinalization::Settled(Ok(ProviderFinalizationKind::RetirementAborted))
        );

        let retry = crate::WorkbenchExecutionControl::from_invocation(Some(invocation.clone()));
        slot.publish_provider_transport(actor, retry.clone());
        slot.accept(&retry);
        assert!(slot
            .retained_operation(key)
            .unwrap()
            .unwrap()
            .same_owner(&original));
        let wrong = WorkbenchRequest::from_cell_input("different effects")
            .with_execution_id(execution.clone())
            .with_checkpoint_boundary(boundary.clone());
        assert_eq!(
            journal.lookup_for_provider_control(&execution, &wrong, Some(key), Some(&retry)),
            Err(WorkbenchReplayFailure::DifferentInput)
        );
        assert!(retry.provider_replay.get().is_none());
        retry.settle_not_admitted(Err(KernelInvocationFailure::Rejected {
            actor,
            receipts: Vec::new(),
            detail: "different input".into(),
            diagnostic: None,
        }));
        slot.complete(&retry);
        assert!(
            slot.retained_boundary(&boundary)
                .unwrap()
                .same_owner(&original),
            "fully rejected physical attempt cannot poison original settlement"
        );
        assert!(matches!(
            original.finalization(),
            HostedOperationFinalization::Settled(Ok(ProviderFinalizationKind::RetirementAborted))
        ));
        let retry = crate::WorkbenchExecutionControl::from_invocation(Some(invocation.clone()));
        slot.publish_provider_transport(actor, retry.clone());
        slot.accept(&retry);
        assert_eq!(
            journal.lookup_for_provider_control(&execution, &request, Some(key), Some(&retry)),
            Ok(Some(reply.clone()))
        );
        retry.settle(reply.clone());
        slot.complete(&retry);
        let recovered = slot.retained_boundary(&boundary).unwrap();
        assert!(original.same_owner(&recovered));
        assert!(
            slot.find(|_| true).is_none(),
            "finished provider custody is not native work"
        );
        assert_eq!(
            recovered.acknowledge().await.unwrap(),
            ProviderFinalizationKind::RetirementAborted
        );
        assert_eq!(
            original.acknowledge().await.unwrap(),
            ProviderFinalizationKind::RetirementAborted
        );
        assert!(
            original.control().is_none(),
            "ack releases large native custody"
        );
        assert!(matches!(
            recovered.terminal(),
            HostedOperationTerminal::Settled(crate::WorkbenchCancellationOutcome::Cancelled { .. })
        ));
        // A fresh physical delivery after ack still resolves through the same
        // exact journal and immutable compact evidence, never a new admission.
        let late = crate::WorkbenchExecutionControl::from_invocation(Some(invocation));
        slot.publish_provider_transport(actor, late.clone());
        slot.accept(&late);
        assert_eq!(
            journal.lookup_for_provider_control(&execution, &request, Some(key), Some(&late)),
            Ok(Some(reply.clone()))
        );
        late.settle(reply);
        slot.complete(&late);
        assert!(slot
            .retained_boundary(&boundary)
            .unwrap()
            .same_owner(&original));
        assert_eq!(
            slot.retained_boundary(&boundary)
                .unwrap()
                .acknowledge()
                .await
                .unwrap(),
            ProviderFinalizationKind::RetirementAborted
        );
    }

    #[test]
    fn abort_owner_refuses_nested_and_foreign_operations_before_checkpoint_extraction() {
        use tidepool_codegen::scope::ScopeId;
        use tidepool_repr::SessionId;
        use tidepool_runtime::session::ContextCheckpointBoundary;

        let journal = Arc::new(Mutex::new(WorkbenchExecutions::default()));
        let checkpoints = crate::ActorAdmissionRegistry::new();
        let actor = crate::ActorRef::first(crate::ActorId(1));
        let boundary =
            ContextCheckpointBoundary::external("thread".into(), "turn".into(), "call".into());
        assert!(
            WorkbenchExecutions::boundary_abort_owner(&journal, &boundary, || false)
                .unwrap()
                .is_none()
        );
        let token = checkpoints.capture_checkpoint(
            "research".into(),
            actor,
            crate::ActorCapabilities::default(),
            None,
            None,
            crate::CheckpointSourceLayer::default(),
            SessionId(7),
            ScopeId(3),
            boundary.clone(),
        );
        for (index, (thread, request, local, original, namespace)) in [
            ("thread", "turn", "call", "call", Some("haskell")),
            ("thread", "turn", "inner", "call", None),
            ("other-thread", "turn", "call", "call", None),
            ("thread", "other-turn", "call", "call", None),
            ("thread", "turn", "other-call", "other-call", None),
        ]
        .into_iter()
        .enumerate()
        {
            let invocation = crate::resident_tools::WorkbenchCallKey::from(
                exomonad_tool::ToolInvocationContext::external(
                    thread.into(),
                    request.into(),
                    local.into(),
                    Some(original.into()),
                    namespace.map(str::to_owned),
                ),
            );
            let execution = WorkbenchExecutionId::from_digest([index as u8; 16]);
            journal.lock().begin(
                &execution,
                WorkbenchRequest::from_cell_input("pure ()").with_execution_id(execution.clone()),
                Some(&invocation),
            );
            let error = WorkbenchExecutions::boundary_abort_owner(&journal, &boundary, || {
                assert!(
                    journal.try_lock().is_some(),
                    "pending-work observation must run outside the journal lock"
                );
                true
            })
            .err()
            .expect("pending custody needs its exact original owner");
            assert_eq!(
                error.detail,
                "output abort has no exact admitted invocation owner"
            );
            assert!(checkpoints.checkpoint(&token, SessionId(7)).is_ok());
        }
    }

    #[test]
    fn abort_cleanup_survives_retry_without_reextracting_obligations() {
        use tidepool_codegen::scope::ScopeId;
        use tidepool_repr::SessionId;
        use tidepool_runtime::session::ContextCheckpointBoundary;

        let journal = Arc::new(Mutex::new(WorkbenchExecutions::default()));
        let invocation = crate::resident_tools::WorkbenchCallKey::from(
            exomonad_tool::ToolInvocationContext::external(
                "thread".into(),
                "turn".into(),
                "call".into(),
                Some("call".into()),
                None,
            ),
        );
        let execution = WorkbenchExecutionId::from_digest([9; 16]);
        journal.lock().begin(
            &execution,
            WorkbenchRequest::from_cell_input("pure ()").with_execution_id(execution.clone()),
            Some(&invocation),
        );
        let actor = crate::ActorRef::first(crate::ActorId(1));
        let checkpoints = crate::ActorAdmissionRegistry::new();
        let boundary =
            ContextCheckpointBoundary::external("thread".into(), "turn".into(), "call".into());
        let token = checkpoints.capture_checkpoint(
            "research".into(),
            actor,
            crate::ActorCapabilities::default(),
            None,
            None,
            crate::CheckpointSourceLayer::default(),
            SessionId(7),
            ScopeId(3),
            boundary.clone(),
        );
        let owner = WorkbenchExecutions::boundary_abort_owner(&journal, &boundary, || true)
            .unwrap()
            .unwrap();
        let cleanup = owner.collect_cleanup(|| {
            assert!(
                journal.try_lock().is_some(),
                "checkpoint extraction must run unlocked"
            );
            BoundaryAbortCleanup {
                children: Vec::new(),
                scopes: checkpoints
                    .settle_checkpoints(actor, &boundary, false)
                    .into_iter()
                    .map(|(_, scope)| scope)
                    .collect(),
            }
        });
        assert_eq!(cleanup.scopes, vec![ScopeId(3)]);
        assert!(matches!(
            checkpoints.checkpoint(&token, SessionId(7)),
            Err(crate::CheckpointRefusal::CaptureFailed)
        ));
        drop(owner);
        // A failed retirement leaves the exact obligation in the admitted journal.
        let retry = WorkbenchExecutions::boundary_abort_owner(&journal, &boundary, || true)
            .unwrap()
            .unwrap();
        let mut cleanup = retry.collect_cleanup(|| panic!("retry must not detach custody again"));
        assert_eq!(cleanup.scopes, vec![ScopeId(3)]);
        cleanup.scopes.clear();
        retry.retain_cleanup(cleanup);
        let completed = retry.collect_cleanup(|| panic!("completed abort remains idempotent"));
        assert!(completed.scopes.is_empty());
    }

    #[test]
    fn recovered_workbench_fences_unsettled_native_calls_and_conflicting_input() {
        let invocation = crate::resident_tools::WorkbenchCallKey::from(
            exomonad_tool::ToolInvocationContext::external(
                "thread".into(),
                "turn".into(),
                "call".into(),
                Some("outer".into()),
                None,
            ),
        );
        let original = WorkbenchExecutionId::from_digest([1; 16]);
        let successor = WorkbenchExecutionId::from_digest([2; 16]);
        let request = WorkbenchRequest::from_cell_input("effectfulAction")
            .with_execution_id(original.clone());
        let mut journal = WorkbenchExecutions::default();
        journal.begin(&original, request.clone(), Some(&invocation));
        let retry = request.with_execution_id(successor.clone());
        assert_eq!(
            journal.lookup(&successor, &retry, Some(&invocation)),
            Err(super::WorkbenchReplayFailure::Unconfirmed)
        );
        let changed = WorkbenchRequest::from_cell_input("differentAction")
            .with_execution_id(successor.clone());
        assert_eq!(
            journal.lookup(&successor, &changed, Some(&invocation)),
            Err(super::WorkbenchReplayFailure::DifferentInput)
        );
        assert!(matches!(journal.cancellation(successor, Some(&invocation)),
            crate::WorkbenchCancellationOutcome::Unconfirmed { execution } if execution == original));
    }

    #[test]
    fn actor_owned_workbench_retry_returns_only_the_exact_committed_call() {
        let execution = WorkbenchExecutionId::from_digest([7; 16]);
        let request = WorkbenchRequest::from_cell_input("effectfulAction")
            .with_execution_id(execution.clone());
        let reply = Ok(WorkbenchResponse {
            status: WorkbenchRunStatus::Committed,
            summary: None,
            items: Vec::new(),
            next_index: 1,
            total: 1,
            publication: None,
        });
        let mut completed = WorkbenchExecutions::default();
        let cancellation = crate::WorkbenchCancellationOutcome::Expired {
            execution: execution.clone(),
            reply: reply.clone(),
        };
        completed.record(
            execution.clone(),
            request.clone(),
            reply.clone(),
            cancellation,
            None,
        );

        assert_eq!(
            completed.lookup(&execution, &request, None),
            Ok(Some(reply))
        );
        assert!(matches!(
            completed.cancellation(execution.clone(), None),
            crate::WorkbenchCancellationOutcome::Expired { .. }
        ));
        let different = WorkbenchRequest::from_cell_input("differentAction")
            .with_execution_id(execution.clone());
        assert_eq!(
            completed.lookup(&execution, &different, None),
            Err(super::WorkbenchReplayFailure::DifferentInput)
        );
        assert_eq!(
            completed.lookup(&WorkbenchExecutionId::from_digest([8; 16]), &request, None),
            Ok(None)
        );
    }

    #[test]
    fn cell_terminal_gates_checkpoint_publication_until_confirmed_full_return() {
        let invocation = crate::resident_tools::WorkbenchCallKey::from(
            exomonad_tool::ToolInvocationContext::external(
                "thread".into(),
                "turn".into(),
                "call".into(),
                Some("call".into()),
                None,
            ),
        );
        let boundary = tidepool_runtime::session::ContextCheckpointBoundary::external(
            "thread".into(),
            "turn".into(),
            "call".into(),
        );
        let execution = WorkbenchExecutionId::from_digest([9; 16]);
        let request =
            WorkbenchRequest::from_cell_input("pure ()").with_execution_id(execution.clone());
        let mut journal = WorkbenchExecutions::default();
        journal.begin(&execution, request, Some(&invocation));
        assert!(
            !journal.cell_allows_publication(&boundary),
            "admitted call has no terminal yet"
        );
        let mut exit = crate::CellExit {
            execution: execution.clone(),
            cause: crate::CellExitCause::FullReturn,
            cleanup_confirmed: false,
        };
        journal.retain_cell_terminal(&execution, Some(&invocation), exit.clone());
        assert!(!journal.cell_allows_publication(&boundary));
        exit.cleanup_confirmed = true;
        journal.retain_cell_terminal(&execution, Some(&invocation), exit);
        assert!(journal.cell_allows_publication(&boundary));
        let sibling = tidepool_runtime::session::ContextCheckpointBoundary::external(
            "thread".into(),
            "turn".into(),
            "sibling".into(),
        );
        assert!(journal.cell_allows_publication(&sibling));
    }

    #[test]
    fn ordinary_hosted_cell_terminal_refuses_failure_cancellation_and_incomplete_returns() {
        let invocation = crate::resident_tools::WorkbenchCallKey::from(
            exomonad_tool::ToolInvocationContext::external(
                "thread".into(),
                "turn".into(),
                "call".into(),
                Some("call".into()),
                None,
            ),
        );
        let boundary = tidepool_runtime::session::ContextCheckpointBoundary::external(
            "thread".into(),
            "turn".into(),
            "call".into(),
        );
        let execution = WorkbenchExecutionId::from_digest([10; 16]);
        let actor = crate::ActorRef::first(crate::ActorId(1));
        let failed = Err(crate::KernelInvocationFailure::Failed {
            receipts: Vec::new(),
            actor,
            detail: "intentional runtime failure".into(),
            diagnostic: None,
        });
        let success = || {
            Ok(crate::KernelStep::Continue(WorkbenchResponse {
                publication: None,
                status: WorkbenchRunStatus::Committed,
                summary: None,
                items: Vec::new(),
                next_index: 1,
                total: 1,
            }))
        };
        let partial = Ok(crate::KernelStep::Continue(WorkbenchResponse {
            publication: None,
            status: WorkbenchRunStatus::Committed,
            summary: None,
            items: Vec::new(),
            next_index: 1,
            total: 2,
        }));
        for (reply, cleanup, cancelled, permits) in [
            (failed, true, false, false),
            (success(), true, true, false),
            (success(), false, false, false),
            (partial, true, false, false),
            (success(), true, false, true),
        ] {
            let control = crate::WorkbenchExecutionControl::untracked();
            assert!(!control.has_context_binding());
            let request =
                WorkbenchRequest::from_cell_input("pure True").with_execution_id(execution.clone());
            let mut journal = WorkbenchExecutions::default();
            journal.begin(&execution, request.clone(), Some(&invocation));
            let exit = crate::CellExit::from_reply(execution.clone(), &reply, cleanup, cancelled);
            journal.retain_cell_terminal(&execution, Some(&invocation), exit);
            // Recording the ordinary reply must retain the publication decision.
            let reply = reply.map(|step| match step {
                crate::KernelStep::Continue(response) => response,
                _ => unreachable!(),
            });
            journal.record(
                execution.clone(),
                request,
                reply,
                crate::WorkbenchCancellationOutcome::NotSleeping {
                    execution: execution.clone(),
                },
                Some(&invocation),
            );
            assert_eq!(journal.cell_allows_publication(&boundary), permits);
        }
    }

    struct CutoffBinding(parking_lot::Mutex<Option<crate::CellExit>>);

    impl crate::HostedContextBinding for CutoffBinding {
        fn admit(
            &self,
            _: &WorkbenchExecutionId,
            _: &exomonad_tool::ToolInvocationContext,
            _: tidepool_repr::PrincipalId,
        ) -> Result<(), tidepool_effect::error::EffectError> {
            Ok(())
        }
        fn prepare(
            &self,
            _: crate::ContextReq,
            _: tidepool_repr::PrincipalId,
            _: tidepool_repr::DataConTable,
        ) -> tidepool_effect::DeferredEffect {
            panic!("cutoff test does not evaluate effects")
        }
        fn cancel(&self) {}
        fn finish(&self, exit: crate::CellExit) {
            *self.0.lock() = Some(exit);
        }
    }

    #[test]
    fn context_binding_and_invocation_ledger_share_the_same_sealed_cell_exit() {
        use crate::HostedContextBinding;
        let invocation = crate::resident_tools::WorkbenchCallKey::from(
            exomonad_tool::ToolInvocationContext::external(
                "thread".into(),
                "turn".into(),
                "call".into(),
                Some("call".into()),
                None,
            ),
        );
        let boundary = tidepool_runtime::session::ContextCheckpointBoundary::external(
            "thread".into(),
            "turn".into(),
            "call".into(),
        );
        let execution = WorkbenchExecutionId::from_digest([26; 16]);
        for cancelled_before_finish in [false, true] {
            let control = crate::WorkbenchExecutionControl::untracked();
            let binding = Arc::new(CutoffBinding(parking_lot::Mutex::new(None)));
            control.bind_context(binding.clone());
            if cancelled_before_finish {
                assert!(control.request_cancellation());
            }
            let result = Ok(crate::KernelStep::Continue(WorkbenchResponse {
                publication: None,
                status: WorkbenchRunStatus::Committed,
                summary: None,
                items: Vec::new(),
                next_index: 1,
                total: 1,
            }));
            let exit = control.finish_cell(execution.clone(), &result, true);
            let request =
                WorkbenchRequest::from_cell_input("pure True").with_execution_id(execution.clone());
            let mut journal = WorkbenchExecutions::default();
            journal.begin(&execution, request, Some(&invocation));
            journal.retain_cell_terminal(&execution, Some(&invocation), exit.clone());
            binding.finish(exit.clone());
            assert!(
                !control.request_cancellation(),
                "the whole-cell cutoff is already sealed"
            );
            assert_eq!(*binding.0.lock(), Some(exit.clone()));
            assert_eq!(
                journal
                    .0
                    .get(&WorkbenchReplayKey::new(&execution, Some(&invocation)))
                    .unwrap()
                    .cell_terminal,
                Some(exit),
            );
            assert_eq!(
                journal.cell_allows_publication(&boundary),
                !cancelled_before_finish
            );
        }
    }

    #[test]
    fn interrupted_recovery_reads_only_the_exact_terminal_execution() {
        let invocation = crate::resident_tools::WorkbenchCallKey::from(
            exomonad_tool::ToolInvocationContext::external(
                "thread".into(),
                "turn".into(),
                "outer".into(),
                Some("outer".into()),
                None,
            ),
        );
        let execution = WorkbenchExecutionId::from_digest([3; 16]);
        let request =
            WorkbenchRequest::from_cell_input("unfold work").with_execution_id(execution.clone());
        let reply = Ok(WorkbenchResponse {
            status: WorkbenchRunStatus::Committed,
            summary: None,
            items: Vec::new(),
            next_index: 1,
            total: 1,
            publication: None,
        });
        let mut journal = WorkbenchExecutions::default();
        journal.begin(&execution, request.clone(), Some(&invocation));
        assert!(matches!(
            journal.at_boundary(
                &tidepool_runtime::session::ContextCheckpointBoundary::external(
                    "thread".into(),
                    "turn".into(),
                    "outer".into()
                )
            ),
            Some(WorkbenchBoundaryRecord::Unconfirmed)
        ));
        journal.record(
            execution.clone(),
            request,
            reply.clone(),
            crate::WorkbenchCancellationOutcome::NotSleeping {
                execution: execution.clone(),
            },
            Some(&invocation),
        );
        assert!(matches!(
            journal.at_boundary(&tidepool_runtime::session::ContextCheckpointBoundary::external("thread".into(), "turn".into(), "outer".into())),
            Some(WorkbenchBoundaryRecord::Terminal(found)) if found == reply
        ));
        assert!(journal
            .at_boundary(
                &tidepool_runtime::session::ContextCheckpointBoundary::external(
                    "thread".into(),
                    "turn".into(),
                    "other".into()
                )
            )
            .is_none());
        assert!(journal
            .at_boundary(
                &tidepool_runtime::session::ContextCheckpointBoundary::external(
                    "thread".into(),
                    "later-turn".into(),
                    "outer".into()
                )
            )
            .is_none());
    }

    #[test]
    fn several_nested_replies_do_not_prove_the_original_operation_result() {
        let mut journal = WorkbenchExecutions::default();
        let boundary = tidepool_runtime::session::ContextCheckpointBoundary::external(
            "thread".into(),
            "turn".into(),
            "outer".into(),
        );
        for (index, call) in ["nested-a", "nested-b"].into_iter().enumerate() {
            let invocation = crate::resident_tools::WorkbenchCallKey::from(
                exomonad_tool::ToolInvocationContext::external(
                    "thread".into(),
                    "turn".into(),
                    call.into(),
                    Some("outer".into()),
                    None,
                ),
            );
            let execution = WorkbenchExecutionId::from_digest([index as u8; 16]);
            let request =
                WorkbenchRequest::from_cell_input("pure ()").with_execution_id(execution.clone());
            let reply = Ok(WorkbenchResponse {
                status: WorkbenchRunStatus::Committed,
                summary: Some(call.into()),
                items: Vec::new(),
                next_index: 1,
                total: 1,
                publication: None,
            });
            journal.record(
                execution.clone(),
                request,
                reply,
                crate::WorkbenchCancellationOutcome::NotSleeping { execution },
                Some(&invocation),
            );
            assert!(matches!(
                journal.at_boundary(&boundary),
                Some(WorkbenchBoundaryRecord::Unconfirmed)
            ));
        }
        assert!(matches!(
            journal.at_boundary(&boundary),
            Some(WorkbenchBoundaryRecord::Unconfirmed)
        ));
    }
}
