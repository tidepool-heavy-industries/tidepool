use super::*;

#[derive(Clone)]
struct WorkbenchExecutionRecord {
    request: WorkbenchRequest,
    state: WorkbenchExecutionState,
    invocation_work: Option<Arc<InvocationWork>>,
    cell_terminal: Option<(crate::CellExit, Arc<crate::WorkbenchExecutionControl>)>,
    boundary_abort: Option<BoundaryAbortCleanup>,
}

#[derive(Clone, Default)]
pub(super) struct BoundaryAbortCleanup {
    pub children: Vec<ActorRef>,
    pub scopes: Vec<tidepool_codegen::scope::ScopeId>,
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
            .collect()
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
            },
        );
    }

    pub(super) fn boundary_abort(
        &self,
        boundary: &tidepool_runtime::session::WorkbenchForkBoundary,
    ) -> Option<BoundaryAbortCleanup> {
        self.0.iter().find_map(|(key, record)| match key {
            WorkbenchReplayKey::Hosted(invocation)
                if invocation.is_original_invocation() && invocation.matches_boundary(boundary) =>
            {
                record.boundary_abort.clone()
            }
            _ => None,
        })
    }

    pub(super) fn retain_boundary_abort(
        &mut self,
        boundary: &tidepool_runtime::session::WorkbenchForkBoundary,
        cleanup: BoundaryAbortCleanup,
    ) -> Result<(), KernelBehaviorError> {
        let record = self.0.iter_mut().find_map(|(key, record)| match key {
            WorkbenchReplayKey::Hosted(invocation)
                if invocation.is_original_invocation() && invocation.matches_boundary(boundary) =>
            {
                Some(record)
            }
            _ => None,
        });
        if let Some(record) = record {
            record.boundary_abort = Some(cleanup);
            return Ok(());
        }
        if cleanup.children.is_empty() && cleanup.scopes.is_empty() {
            return Ok(());
        }
        Err(KernelBehaviorError {
            detail: "output abort has no exact admitted invocation owner".into(),
        })
    }

    pub(super) fn retain_cell_terminal(
        &mut self,
        execution: &WorkbenchExecutionId,
        invocation: Option<&crate::resident_tools::WorkbenchCallKey>,
        exit: crate::CellExit,
        control: Arc<crate::WorkbenchExecutionControl>,
    ) {
        let record = self
            .0
            .get_mut(&WorkbenchReplayKey::new(execution, invocation))
            .expect("cell terminal follows admitted execution");
        record.cell_terminal = Some((exit, control));
    }

    pub(super) fn cell_allows_publication(
        &self,
        boundary: &tidepool_runtime::session::WorkbenchForkBoundary,
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
                    .is_some_and(|(exit, control)| {
                        exit.permits_context_commit() && !control.context_cancellation_requested()
                    })
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
        boundary: &tidepool_runtime::session::WorkbenchForkBoundary,
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
    fn cell_terminal_gates_deferred_publication_and_late_cancellation() {
        let invocation = crate::resident_tools::WorkbenchCallKey::from(
            exomonad_tool::ToolInvocationContext::external(
                "thread".into(),
                "turn".into(),
                "call".into(),
                Some("call".into()),
                None,
            ),
        );
        let boundary = tidepool_runtime::session::WorkbenchForkBoundary::external(
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
        let control = crate::WorkbenchExecutionControl::untracked();
        let mut exit = crate::CellExit {
            execution: execution.clone(),
            cause: crate::CellExitCause::FullReturn,
            cleanup_confirmed: false,
        };
        journal.retain_cell_terminal(&execution, Some(&invocation), exit.clone(), control.clone());
        assert!(!journal.cell_allows_publication(&boundary));
        exit.cleanup_confirmed = true;
        journal.retain_cell_terminal(&execution, Some(&invocation), exit, control.clone());
        assert!(journal.cell_allows_publication(&boundary));
        control.publication_decision().claim_commit().unwrap();
        assert!(!control.request_cancellation());
        assert!(!journal.cell_allows_publication(&boundary));
        let sibling = tidepool_runtime::session::WorkbenchForkBoundary::external(
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
        let boundary = tidepool_runtime::session::WorkbenchForkBoundary::external(
            "thread".into(),
            "turn".into(),
            "call".into(),
        );
        let execution = WorkbenchExecutionId::from_digest([10; 16]);
        let actor = crate::ActorRef::first(crate::ActorId(1));
        let failed = Err(crate::KernelInvocationFailure::Failed {
            actor,
            detail: "intentional runtime failure".into(),
        });
        let success = || {
            Ok(crate::KernelStep::Continue(WorkbenchResponse {
                status: WorkbenchRunStatus::Committed,
                summary: None,
                items: Vec::new(),
                next_index: 1,
                total: 1,
            }))
        };
        let partial = Ok(crate::KernelStep::Continue(WorkbenchResponse {
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
            let request = WorkbenchRequest::from_cell_input("unfoldDeferred group branches")
                .with_execution_id(execution.clone());
            let mut journal = WorkbenchExecutions::default();
            journal.begin(&execution, request.clone(), Some(&invocation));
            let exit = crate::CellExit::from_reply(execution.clone(), &reply, cleanup, cancelled);
            journal.retain_cell_terminal(&execution, Some(&invocation), exit, control);
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
        });
        let mut journal = WorkbenchExecutions::default();
        journal.begin(&execution, request.clone(), Some(&invocation));
        assert!(matches!(
            journal.at_boundary(&tidepool_runtime::session::WorkbenchForkBoundary::external(
                "thread".into(),
                "turn".into(),
                "outer".into()
            )),
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
            journal.at_boundary(&tidepool_runtime::session::WorkbenchForkBoundary::external("thread".into(), "turn".into(), "outer".into())),
            Some(WorkbenchBoundaryRecord::Terminal(found)) if found == reply
        ));
        assert!(journal
            .at_boundary(&tidepool_runtime::session::WorkbenchForkBoundary::external(
                "thread".into(),
                "turn".into(),
                "other".into()
            ))
            .is_none());
        assert!(journal
            .at_boundary(&tidepool_runtime::session::WorkbenchForkBoundary::external(
                "thread".into(),
                "later-turn".into(),
                "outer".into()
            ))
            .is_none());
    }

    #[test]
    fn several_nested_replies_do_not_prove_the_original_operation_result() {
        let mut journal = WorkbenchExecutions::default();
        let boundary = tidepool_runtime::session::WorkbenchForkBoundary::external(
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
