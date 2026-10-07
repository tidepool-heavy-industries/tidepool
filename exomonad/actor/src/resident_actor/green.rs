//! Invocation-local frontiers over the existing actor execution task. Native
//! continuations and Haskell result cells remain owned by the resident machine.

use super::*;
use futures_util::future::{poll_fn, BoxFuture};
use std::collections::BTreeMap;
use std::task::Poll;

#[cfg(test)]
#[path = "green_tests.rs"]
mod tests;

pub(crate) enum GreenBoundary {
    Spawn { continuation: ResidentHole, callback: RootCustody },
    Done { continuation: ResidentHole, token: i64 },
    JoinAny { continuation: ResidentHole, threads: Vec<i64> },
    Status { continuation: ResidentHole, thread: i64 },
    Cancel { continuation: ResidentHole, thread: i64 },
}

impl GreenBoundary {
    pub(crate) fn operation(&self) -> &'static str {
        match self {
            Self::Spawn { .. } => "async",
            Self::Done { .. } => "async completion",
            Self::JoinAny { .. } => "async join",
            Self::Status { .. } => "async status",
            Self::Cancel { .. } => "async cancel",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ThreadStatus { Running, Settled, Cancelled }

struct Thread {
    parent: i64,
    work: Arc<InvocationWork>,
    realm: RealmId,
    status: ThreadStatus,
    control: Arc<crate::WorkbenchExecutionControl>,
}

pub(super) struct EffectReceipt {
    display: Option<WorkbenchDisplayOutput>,
    success_disposition: WorkbenchOperationDisposition,
    ordinal: usize,
    effect: String,
    started: std::time::Instant,
    observed_child: Option<ActorRef>,
}

pub(super) struct Completion {
    pub thread: i64,
    pub scopes: Vec<scopes::ScopeFrame>,
    pub result: Option<commands::CommandResolution>,
    receipt: Option<EffectReceipt>,
    terminal: Vec<(i64, ThreadStatus)>,
}

struct Pending {
    thread: i64,
    future: BoxFuture<'static, Completion>,
}

struct Join {
    thread: i64,
    continuation: ResidentHole,
    threads: Vec<i64>,
    scopes: Vec<scopes::ScopeFrame>,
}

#[derive(Default)]
pub(super) struct GreenThreads {
    active: i64,
    threads: BTreeMap<i64, Thread>,
    pending: Vec<Pending>,
    joins: Vec<Join>,
}

impl GreenThreads {
    pub(super) fn active_work(&self) -> Option<Arc<InvocationWork>> {
        self.threads.get(&self.active).map(|thread| thread.work.clone())
    }

    pub(super) fn active_realm(&self) -> Option<RealmId> {
        self.threads.get(&self.active).map(|thread| thread.realm)
    }

    pub(super) fn active_wait_control(&self) -> Option<Arc<crate::WorkbenchExecutionControl>> {
        self.threads.get(&self.active).map(|thread| thread.control.clone())
    }

    fn status(&self, thread: i64) -> Result<ThreadStatus, ResidentActorWorkbenchError> {
        self.threads.get(&thread).map(|thread| thread.status).ok_or_else(|| protocol(
            "async handle does not belong to this invocation",
        ))
    }

    fn winner(&self, threads: &[i64]) -> Result<Option<i64>, ResidentActorWorkbenchError> {
        if threads.is_empty() { return Err(protocol("async join requires a nonempty list")); }
        let mut winner = None;
        for &thread in threads {
            if self.status(thread)? != ThreadStatus::Running && winner.is_none() {
                winner = Some(thread);
            }
        }
        Ok(winner)
    }

    fn enqueue(
        &mut self,
        scopes: Vec<scopes::ScopeFrame>,
        operation: BoxFuture<'static, Result<ResidentOutcome, ResidentActorWorkbenchError>>,
    ) {
        let thread = self.active;
        self.pending.push(Pending { thread, future: Box::pin(async move {
            let outcome = operation.await;
            Completion {
                thread, scopes, receipt: None, terminal: Vec::new(),
                result: Some(commands::CommandResolution {
                    disposition: WorkbenchOperationDisposition::Committed,
                    outcome, started_job: None, retained_job_binding: None,
                }),
            }
        }) });
    }

    pub(super) fn enqueue_effect(
        &mut self,
        scopes: Vec<scopes::ScopeFrame>,
        receipt: EffectReceipt,
        future: BoxFuture<'static, commands::CommandResolution>,
    ) {
        let thread = self.active;
        self.pending.push(Pending { thread, future: Box::pin(async move {
            Completion {
                thread, scopes, result: Some(future.await), receipt: Some(receipt), terminal: Vec::new(),
            }
        }) });
    }

    pub(super) async fn next(&mut self) -> Result<Completion, ResidentActorWorkbenchError> {
        if self.pending.is_empty() { return Err(protocol("async computations have no runnable effect frontier")); }
        poll_fn(|cx| {
            for index in 0..self.pending.len() {
                if let Poll::Ready(completion) = self.pending[index].future.as_mut().poll(cx) {
                    drop(self.pending.remove(index));
                    return Poll::Ready(Ok(completion));
                }
            }
            Poll::Pending
        }).await
    }

    pub(super) fn cancel_parent(&mut self) {
        for thread in self.threads.values() { thread.work.close(); }
        self.pending.clear();
        self.joins.clear();
    }

    /// Closing a thread's native scope also closes its nested thread scopes.
    /// Remove their pending service futures before any stale result can resume.
    fn remove_descendant_frontiers(&mut self, parent: i64) -> Vec<i64> {
        let mut descendants = vec![parent];
        let mut index = 0;
        while index < descendants.len() {
            let ancestor = descendants[index];
            descendants.extend(self.threads.iter().filter_map(|(&id, thread)| {
                (thread.parent == ancestor && thread.status == ThreadStatus::Running).then_some(id)
            }));
            index += 1;
        }
        self.pending.retain(|pending| !descendants.contains(&pending.thread));
        self.joins.retain(|join| !descendants.contains(&join.thread));
        descendants
    }
}

impl EffectReceipt {
    pub(super) fn split(pending: ParkedWorkbenchEffect) -> (OwnedWorkbenchWait, Self) {
        let observed_child = pending.wait.observe_after_resume();
        (pending.wait, Self {
            display: pending.display,
            success_disposition: pending.success_disposition,
            ordinal: pending.ordinal,
            effect: pending.effect,
            started: pending.started,
            observed_child,
        })
    }
}

impl<H, O> ResidentKernelBehavior<H, O>
where H: DispatchEffect<O> + Send + 'static, O: OutputSink + Sync + 'static,
{
    pub(super) fn prepare_green_boundary(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        owner: &CurrentEffectOwner<'_>,
        current: &mut WorkbenchFragmentExecution,
        boundary: GreenBoundary,
    ) -> Result<FragmentAdvance, ResidentActorWorkbenchError> {
        let mut green = current.green.take().unwrap_or_default();
        let runner = self.environment.runner.clone();
        let resume_context = context.clone();
        match boundary {
            GreenBoundary::Spawn { continuation, callback } => {
                let (work, realm) = self.register_resource_scope(context, owner)
                    .map_err(|detail| protocol(&detail))?;
                let token = work.scope_token().expect("native child resource identity");
                green.enqueue(std::mem::take(&mut current.scopes), Box::pin(async move {
                    runner.resume_value(resume_context, continuation, token).await
                }));
                green.threads.insert(token, Thread {
                    parent: green.active, work: work.clone(), realm, status: ThreadStatus::Running,
                    control: crate::WorkbenchExecutionControl::untracked(),
                });
                green.active = token;
                current.native_start = Some(owned_workbench::WorkbenchFragmentRequest::ScopeStart {
                    fragment: current.fragment.take().expect("async child borrows original fragment"),
                    callback, realm, token, work,
                });
                current.green = Some(green);
                return Ok(FragmentAdvance::ParkNative);
            }
            GreenBoundary::Done { continuation, token } => {
                if green.active == 0 || token != green.active || !current.scopes.is_empty() {
                    return Err(protocol("async completion does not match its current thread delimiter"));
                }
                let work = green.active_work().expect("active native thread");
                let thread = green.active;
                let descendants = green.remove_descendant_frontiers(thread);
                let environment = self.environment.clone();
                let kernel = kernel.clone();
                green.pending.push(Pending { thread, future: Box::pin(async move {
                    // Closing the native scope retires the payload-free Done frame.
                    drop(continuation);
                    let closed = close_thread(environment, kernel, work).await;
                    Completion {
                        thread, scopes: Vec::new(), receipt: None,
                        terminal: if closed.is_ok() { descendants.into_iter().map(|id| {
                            (id, if id == thread { ThreadStatus::Settled } else { ThreadStatus::Cancelled })
                        }).collect() } else { Vec::new() },
                        result: closed.err().map(|error| commands::CommandResolution {
                            outcome: Err(error), disposition: WorkbenchOperationDisposition::Unknown,
                            started_job: None, retained_job_binding: None,
                        }),
                    }
                }) });
            }
            GreenBoundary::Status { continuation, thread } => {
                let status = match green.status(thread)? {
                    ThreadStatus::Running => 0_i64,
                    ThreadStatus::Settled => 1,
                    ThreadStatus::Cancelled => 2,
                };
                green.enqueue(std::mem::take(&mut current.scopes), Box::pin(async move {
                    runner.resume_value(resume_context, continuation, status).await
                }));
            }
            GreenBoundary::JoinAny { continuation, threads } => {
                let winner = green.winner(&threads)?;
                let scopes = std::mem::take(&mut current.scopes);
                match winner {
                    Some(winner) => green.enqueue(scopes, Box::pin(async move {
                        runner.resume_value(resume_context, continuation, winner).await
                    })),
                    None => green.joins.push(Join {
                        thread: green.active, continuation, threads, scopes,
                    }),
                }
            }
            GreenBoundary::Cancel { continuation, thread } => {
                if green.status(thread)? != ThreadStatus::Running {
                    green.enqueue(std::mem::take(&mut current.scopes), Box::pin(async move {
                        runner.resume_unit(resume_context, continuation).await
                    }));
                } else {
                    let descendants = green.remove_descendant_frontiers(thread);
                    let work = green.threads[&thread].work.clone();
                    work.close();
                    let caller = green.active;
                    let scopes = std::mem::take(&mut current.scopes);
                    let environment = self.environment.clone();
                    let kernel = kernel.clone();
                    green.pending.push(Pending { thread: caller, future: Box::pin(async move {
                        let closed = close_thread(environment, kernel, work).await;
                        let terminal = if closed.is_ok() {
                            descendants.into_iter().map(|id| (id, ThreadStatus::Cancelled)).collect()
                        } else { Vec::new() };
                        let result = if caller == thread {
                            drop(continuation);
                            closed.err().map(|error| commands::CommandResolution {
                                outcome: Err(error), disposition: WorkbenchOperationDisposition::Unknown,
                                started_job: None, retained_job_binding: None,
                            })
                        } else {
                            Some(commands::CommandResolution {
                                outcome: match closed {
                                    Ok(()) => runner.resume_unit(resume_context, continuation).await,
                                    Err(error) => Err(error),
                                },
                                disposition: WorkbenchOperationDisposition::Committed,
                                started_job: None, retained_job_binding: None,
                            })
                        };
                        Completion { thread: caller, scopes, result, receipt: None, terminal }
                    }) });
                }
            }
        }
        current.green = Some(green);
        Ok(FragmentAdvance::ParkGreen)
    }

    /// Apply one ready frontier under the original owned execution fence.
    pub(super) fn apply_green_completion(
        &mut self,
        state: &mut WorkbenchExecutionState,
        mut completion: Completion,
    ) -> Result<bool, ResidentActorWorkbenchError> {
        let current = state.cursor.running.as_mut().expect("original async fragment");
        let green = current.green.as_mut().expect("invocation-local async frontiers");
        if !completion.terminal.is_empty() {
            for (thread, status) in completion.terminal {
            green.threads.get_mut(&thread).expect("issued async thread").status = status;
            }
            let joins = std::mem::take(&mut green.joins);
            for join in joins {
                match green.winner(&join.threads)? {
                    Some(winner) => {
                        let runner = self.environment.runner.clone();
                        let context = state.effects.context.clone();
                        let previous = green.active;
                        green.active = join.thread;
                        green.enqueue(join.scopes, Box::pin(async move {
                            runner.resume_value(context, join.continuation, winner).await
                        }));
                        green.active = previous;
                    }
                    None => green.joins.push(join),
                }
            }
        }
        let Some(mut result) = completion.result.take() else { return Ok(false); };
        green.active = completion.thread;
        current.scopes = completion.scopes;
        current.inflight_effect = None;
        if let Some(receipt) = completion.receipt {
            if let Some(binding) = result.retained_job_binding.take() {
                if !state.effects.control.as_ref().is_some_and(|control| control.cancellation_requested()) {
                    match binding.accept() {
                        Ok(binding) => {
                            state.cursor.unit.recovered_bindings.push(binding.clone());
                            current.fragment.as_mut().expect("original command fragment").retain_job_binding(binding);
                        }
                        Err(error) => { result.disposition = WorkbenchOperationDisposition::Unknown; result.outcome = Err(error); }
                    }
                }
            }
            record_workbench_operation(
                &mut state.cursor.unit.operations, state.request.execution_id(), state.cursor.index,
                receipt.ordinal, &receipt.effect, receipt.display, receipt.started.elapsed(),
                scoped_operation_disposition(receipt.success_disposition, result.disposition),
            );
            if let Some(job) = result.started_job {
                current.fragment.as_mut().expect("original command fragment").record_started_job(job);
            }
            if result.outcome.is_ok() {
                if let Some(child) = receipt.observed_child { self.record_child_observation(child); }
            }
        }
        match result.outcome {
            Ok(outcome) => current.outcome = Some(outcome),
            Err(error) => current.resume_failure = Some(error),
        }
        Ok(true)
    }
}

fn protocol(detail: &str) -> ResidentActorWorkbenchError {
    ResidentActorWorkbenchError::ActorProtocol(detail.into())
}

async fn close_thread<H, O>(
    environment: ResidentEnvironment<H, O>,
    kernel: KernelContext,
    work: Arc<InvocationWork>,
) -> Result<(), ResidentActorWorkbenchError>
where H: DispatchEffect<O> + Send + 'static, O: OutputSink + Sync + 'static,
{
    work.close();
    let cleanup = work.cleanup(&environment, &kernel).await;
    match cleanup.uncertainty() {
        None => Ok(()),
        Some(detail) => Err(protocol(&format!("async resource cleanup unconfirmed: {detail}"))),
    }
}
