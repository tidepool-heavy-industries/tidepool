use super::*;

#[derive(Clone)]
struct WorkbenchExecutionRecord {
    request: WorkbenchRequest,
    state: WorkbenchExecutionState,
    invocation_work: Option<Arc<InvocationWork>>,
    cell_terminal: Option<crate::CellExit>,
    boundary_abort: Option<BoundaryAbortCleanup>,
    display_settlements: Arc<DisplayExecutionSettlement>,
}

#[derive(Clone, Default)]
pub(super) struct BoundaryAbortCleanup {
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
            },
        );
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
