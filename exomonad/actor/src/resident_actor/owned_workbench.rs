//! Single-admission preparation and watch tasks retain one execution cursor.

use super::*;
use crate::{OwnedWorkbenchCompletion, OwnedWorkbenchTask, WorkbenchAdvance, WorkbenchDispatch};

struct OwnedExecution<H, O> {
    state: WorkbenchExecutionState,
    workbench: crate::ResidentActorWorkbench<H, O>,
    timing: Option<crate::call_timing::CallScope>,
    cleanup: crate::resident_workbench::ParkedHoleAbortGuard,
}

impl<H, O> OwnedExecution<H, O> {
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
    pub(super) fn dispatch_owned_workbench(
        &mut self,
        kernel: &KernelContext,
        invocation: crate::ActorWorkbenchInvocation,
        control: Option<Arc<crate::WorkbenchExecutionControl>>,
    ) -> WorkbenchDispatch<Self> {
        // Builtin repair and inspection paths retain their serial admission.
        if invocation.request.tool_call().is_some_and(|call| {
            matches!(
                call.name.as_str(),
                crate::status_tool::STATUS_TOOL
                    | crate::reload_spec_tool::RELOAD_SPEC_TOOL
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
                return terminal_task(reply.map(KernelStep::Continue))
            }
            Err(error) => return terminal_task(Err(error)),
        };
        let WorkbenchAdmission {
            context,
            request,
            installed_tools,
            admitted_source,
            capture,
            control,
            invocation,
            ..
        } = admitted;
        let Some(workbench) = self.active_workbench() else {
            return terminal_task(Err(KernelInvocationFailure::Rejected {
                actor: context.actor,
                detail: "actor application has no active Haskell workbench".into(),
            }));
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
        let kind = request
            .tool_call()
            .map(|call| call.name.clone())
            .unwrap_or_else(|| "cell".into());
        let (actor, incarnation) = actor_address(context.actor);
        let timing = crate::call_timing::CallScope::new(kind, actor as u64, incarnation as u64);
        let cleanup = workbench.continuation_cleanup_owner(
            context.clone(),
            "hosted execution abandoned before exact continuation settlement".into(),
        );
        let owned = OwnedExecution {
            state: WorkbenchExecutionState {
                effects: WorkbenchEffectState {
                    park_watch: true,
                    context: context.clone(),
                    public_visibility: None,
                    control,
                    installed_tools,
                    admitted_source,
                    reservation_owner,
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
            workbench,
            timing: Some(timing),
            cleanup,
        };
        let runner = self.environment.runner.clone();
        WorkbenchDispatch::Owned(OwnedWorkbenchTask::new(Box::pin(async move {
            let (timing, cleanup) = owned.scopes();
            let prepared = timing
                .scope(cleanup.scope(async {
                    let public = runner.public_visibility_snapshot(context.clone()).await;
                    let cell = match owned.state.request.cell_source().filter(|_| public.is_ok()) {
                        Some(source) => Some(
                            owned
                                .workbench
                                .prepare_cell(context.clone(), source.to_owned())
                                .await,
                        ),
                        None => None,
                    };
                    (public, cell)
                }))
                .await;
            OwnedWorkbenchCompletion::advance(move |behavior: &mut Self, _kernel| {
                let (timing, cleanup) = owned.scopes();
                timing.sync_scope(|| {
                    cleanup.sync_scope(|| {
                        let mut owned = owned;
                        match prepared.0 {
                            Ok(public) => owned.state.effects.public_visibility = Some(public),
                            Err(error) => {
                                return Ok(WorkbenchAdvance::Park(Self::finish_owned_task(
                                    owned,
                                    Err(workbench_failure(&[], 0, 1, error)),
                                )))
                            }
                        }
                        if let Some(cell) = prepared.1 {
                            match install_cell_preparation(
                                &mut owned.state.request,
                                &mut owned.state.cursor,
                                cell,
                            ) {
                                Ok(Some(step)) => {
                                    return Ok(WorkbenchAdvance::Park(Self::finish_owned_task(
                                        owned,
                                        Ok(step),
                                    )))
                                }
                                Err(error) => {
                                    return Ok(WorkbenchAdvance::Park(Self::finish_owned_task(
                                        owned,
                                        Err(error),
                                    )))
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
                    })
                })
            })
        })))
    }

    /// The existing serial driver keeps all unconverted handlers and terminal
    /// cleanup. It resumes this same cursor and yields before awaiting a watch.
    fn continue_owned_task(mut owned: OwnedExecution<H, O>) -> OwnedWorkbenchTask<Self> {
        OwnedWorkbenchTask::serial(move |mut behavior: Self, kernel| {
            Box::pin(async move {
                let (timing, cleanup) = owned.scopes();
                let result = timing
                    .scope(cleanup.scope(behavior.execute_workbench(
                        &kernel,
                        &mut owned.state,
                        Some(&owned.workbench),
                    )))
                    .await;
                match result {
                    Ok(WorkbenchRunAdvance::ParkWatch) => {
                        tracing::debug!(actor = ?owned.state.effects.context.actor,
                        input_unit_index = owned.state.cursor.index, "serial cursor yielded its captured watch");
                        let completion = OwnedWorkbenchCompletion::advance(
                            move |behavior: &mut Self, kernel| {
                                let (timing, cleanup) = owned.scopes();
                                timing.sync_scope(|| {
                                    cleanup.sync_scope(|| {
                                        Ok(WorkbenchAdvance::Park(
                                            behavior.owned_watch_task(owned, kernel.clone()),
                                        ))
                                    })
                                })
                            },
                        );
                        (behavior, completion)
                    }
                    result => {
                        let result = result.map(|advance| match advance {
                            WorkbenchRunAdvance::Complete(step) => step,
                            WorkbenchRunAdvance::ParkWatch => unreachable!("watch handled above"),
                        });
                        Self::finish_owned(behavior, owned, result).await
                    }
                }
            })
        })
    }

    fn owned_watch_task(
        &self,
        mut owned: OwnedExecution<H, O>,
        kernel: KernelContext,
    ) -> OwnedWorkbenchTask<Self> {
        let pending = owned
            .state
            .cursor
            .running
            .as_mut()
            .expect("watch retains running fragment")
            .parked_watch
            .take()
            .expect("one captured watch");
        let environment = self.environment.clone();
        let context = owned.state.effects.context.clone();
        let control = owned
            .state
            .effects
            .control
            .clone()
            .unwrap_or_else(crate::WorkbenchExecutionControl::untracked);
        OwnedWorkbenchTask::new(Box::pin(async move {
            let (timing, cleanup) = owned.scopes();
            let result = timing
                .scope(cleanup.scope(await_watch(
                    environment,
                    kernel,
                    context,
                    control,
                    pending.poll,
                )))
                .await;
            OwnedWorkbenchCompletion::advance(move |_behavior: &mut Self, _kernel| {
                let (timing, cleanup) = owned.scopes();
                timing.sync_scope(|| {
                    cleanup.sync_scope(|| {
                        record_workbench_operation(
                            &mut owned.state.cursor.unit.operations,
                            owned.state.request.execution_id(),
                            owned.state.cursor.index,
                            pending.ordinal,
                            &pending.effect,
                            pending.started.elapsed(),
                            match &result {
                                Ok(_) => WorkbenchOperationDisposition::Committed,
                                Err(error) => disposition_for_non_command_failure(error),
                            },
                        );
                        match result {
                            Ok(outcome) => {
                                owned
                                    .state
                                    .cursor
                                    .running
                                    .as_mut()
                                    .expect("same watched fragment")
                                    .outcome = Some(outcome);
                                Ok(WorkbenchAdvance::Park(Self::continue_owned_task(owned)))
                            }
                            Err(error) => {
                                owned
                                    .state
                                    .cursor
                                    .running
                                    .as_mut()
                                    .expect("same watched fragment")
                                    .resume_failure = Some(error);
                                Ok(WorkbenchAdvance::Park(Self::continue_owned_task(owned)))
                            }
                        }
                    })
                })
            })
        }))
    }

    fn finish_owned_task(
        owned: OwnedExecution<H, O>,
        result: Result<KernelStep<WorkbenchResponse>, WorkbenchExecutionFailure>,
    ) -> OwnedWorkbenchTask<Self> {
        OwnedWorkbenchTask::serial(move |behavior, _kernel| {
            Box::pin(Self::finish_owned(behavior, owned, result))
        })
    }

    async fn finish_owned(
        mut behavior: Self,
        mut owned: OwnedExecution<H, O>,
        result: Result<KernelStep<WorkbenchResponse>, WorkbenchExecutionFailure>,
    ) -> (Self, OwnedWorkbenchCompletion<Self>) {
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
        let (timing, cleanup) = owned.scopes();
        let result = timing
            .scope(cleanup.scope(behavior.finalize_workbench_execution(&mut owned.state, result)))
            .await;
        owned
            .timing
            .take()
            .expect("one terminal timing owner")
            .finish(&outcome);
        (
            behavior,
            OwnedWorkbenchCompletion::new(move |_behavior| {
                // Keep exact cleanup custody through the fenced mailbox join.
                let _owned = owned;
                result
            }),
        )
    }
}

fn terminal_task<B: 'static>(
    result: Result<KernelStep<WorkbenchResponse>, KernelInvocationFailure>,
) -> WorkbenchDispatch<B> {
    WorkbenchDispatch::Owned(OwnedWorkbenchTask::new(Box::pin(async move {
        OwnedWorkbenchCompletion::new(move |_| result)
    })))
}

pub(super) async fn await_watch<H, O>(
    environment: ResidentEnvironment<H, O>,
    kernel: KernelContext,
    context: ActorSessionContext,
    control: Arc<crate::WorkbenchExecutionControl>,
    poll: crate::request_effect::WatchPoll,
) -> Result<ResidentOutcome, ResidentActorWorkbenchError>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    tracing::debug!(actor = ?context.actor, watch = ?poll.watch, "owned watch awaiting settlement");
    let waiting = environment.requests.await_watch(context.actor, poll.watch);
    tokio::pin!(waiting);
    tokio::select! {
        observation = &mut waiting => {
            tracing::debug!(actor = ?context.actor, watch = ?poll.watch, observation = ?observation,
                "owned watch received settlement");
            if control.claim_expiry() {
                let observation = observation.map(|observation| watch_pending_observation(&environment, poll.watch, observation));
                let outcome = environment.runner.resume_watch_observation(context.clone(), poll.continuation, observation).await;
                control.finish_sleep();
                outcome
            } else {
                let (outcome, consumed) = environment.runner.abort_live(context.clone(), poll.continuation,
                    "awaitWatch interrupted by delivered input".into()).await;
                if consumed { control.acknowledge_cancellation(); }
                outcome
            }
        }
        () = control.wait_for_cancellation() => {
            let (outcome, consumed) = environment.runner.abort_live(context.clone(), poll.continuation,
                "awaitWatch interrupted by delivered input".into()).await;
            if consumed { control.acknowledge_cancellation(); }
            outcome
        }
        terminal = kernel.wait_requested_shutdown() => {
            control.request_cancellation();
            let (outcome, consumed) = environment.runner.abort_live(context.clone(), poll.continuation,
                format!("awaitWatch interrupted by actor retirement: {}", terminal.summary)).await;
            if consumed { control.acknowledge_cancellation(); }
            outcome
        }
    }
}
