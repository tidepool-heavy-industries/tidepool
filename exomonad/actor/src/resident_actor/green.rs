//! Invocation-local frontiers over the existing actor execution task. Native
//! continuations and Haskell result cells remain owned by the resident machine.

use super::*;
use futures_util::future::{maybe_done, poll_fn, BoxFuture, MaybeDone};
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::task::Poll;

#[cfg(test)]
#[path = "green_tests.rs"]
mod tests;

pub(super) enum GreenAdvance {
    Start {
        callback: RootCustody,
        realm: RealmId,
        token: i64,
        work: Arc<InvocationWork>,
    },
    Wait,
}

pub(crate) enum GreenBoundary {
    Spawn {
        continuation: ResidentHole,
        callback: RootCustody,
    },
    Done {
        continuation: ResidentHole,
        token: i64,
    },
    JoinAny {
        continuation: ResidentHole,
        threads: Vec<i64>,
    },
    Status {
        continuation: ResidentHole,
        thread: i64,
    },
    Cancel {
        continuation: ResidentHole,
        thread: i64,
    },
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
enum ThreadStatus {
    Running,
    Settled,
    Cancelled,
}

struct Thread {
    parent: i64,
    work: Arc<InvocationWork>,
    realm: RealmId,
    status: ThreadStatus,
    control: Arc<crate::WorkbenchExecutionControl>,
}

pub(super) struct Completion<A> {
    pub thread: i64,
    pub scopes: Vec<scopes::ScopeFrame>,
    pub result: Option<Result<ResidentOutcome, ResidentActorWorkbenchError>>,
    pub attachment: Option<A>,
    terminal: Vec<(i64, ThreadStatus)>,
}

struct Pending<A> {
    thread: i64,
    future: MaybeDone<BoxFuture<'static, Completion<A>>>,
}

struct Join {
    thread: i64,
    continuation: ResidentHole,
    threads: Vec<i64>,
    scopes: Vec<scopes::ScopeFrame>,
}

pub(super) struct GreenInvocation<A> {
    active: i64,
    threads: BTreeMap<i64, Thread>,
    pending: Vec<Pending<A>>,
    joins: Vec<Join>,
}

impl<A> Default for GreenInvocation<A> {
    fn default() -> Self {
        Self {
            active: 0,
            threads: BTreeMap::new(),
            pending: Vec::new(),
            joins: Vec::new(),
        }
    }
}

/// Owns every retained frontier for one admitted invocation. Native execution
/// always borrows this owner, including the ordinary invocation without Green.
pub(super) struct InvocationExecution<A> {
    green: Option<GreenInvocation<A>>,
}

impl<A> Default for InvocationExecution<A> {
    fn default() -> Self {
        Self { green: None }
    }
}

impl<A: Send + 'static> InvocationExecution<A> {
    #[cfg(test)]
    pub(super) fn from_green(green: GreenInvocation<A>) -> Self {
        Self { green: Some(green) }
    }
    #[cfg(test)]
    pub(super) async fn test_drive<T>(&mut self, operation: impl Future<Output = T>) -> T {
        self.drive(operation).await
    }
    pub(super) fn has_green(&self) -> bool {
        self.green.is_some()
    }
    pub(super) fn active_work(&self) -> Option<Arc<InvocationWork>> {
        self.green.as_ref().and_then(GreenInvocation::active_work)
    }
    pub(super) fn active_realm(&self) -> Option<RealmId> {
        self.green.as_ref().and_then(GreenInvocation::active_realm)
    }
    pub(super) fn active_wait_control(&self) -> Option<Arc<crate::WorkbenchExecutionControl>> {
        self.green
            .as_ref()
            .and_then(GreenInvocation::active_wait_control)
    }
    pub(super) fn cancel_parent(&mut self) {
        if let Some(green) = &mut self.green {
            green.cancel_parent();
        }
    }
    pub(super) fn enqueue_frontier(
        &mut self,
        scopes: Vec<scopes::ScopeFrame>,
        future: BoxFuture<'static, (Result<ResidentOutcome, ResidentActorWorkbenchError>, A)>,
    ) {
        self.green
            .as_mut()
            .expect("issued Green invocation")
            .enqueue_frontier(scopes, future);
    }
    pub(super) async fn next(&mut self) -> Result<Completion<A>, ResidentActorWorkbenchError> {
        self.green
            .as_mut()
            .expect("issued Green invocation")
            .next()
            .await
    }
    pub(super) fn prepare<H, O>(
        &mut self,
        behavior: &mut ResidentKernelBehavior<H, O>,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        owner: &CurrentEffectOwner<'_>,
        scopes: &mut Vec<scopes::ScopeFrame>,
        boundary: GreenBoundary,
    ) -> Result<GreenAdvance, ResidentActorWorkbenchError>
    where
        H: DispatchEffect<O> + Send + 'static,
        O: OutputSink + Sync + 'static,
    {
        behavior.prepare_green_operation(
            kernel,
            context,
            owner,
            self.green.get_or_insert_with(Default::default),
            scopes,
            boundary,
        )
    }
    pub(super) fn apply<H, O>(
        &mut self,
        behavior: &ResidentKernelBehavior<H, O>,
        context: &ActorSessionContext,
        completion: Completion<A>,
    ) -> Result<Option<Completion<A>>, ResidentActorWorkbenchError>
    where
        H: DispatchEffect<O> + Send + 'static,
        O: OutputSink + Sync + 'static,
    {
        behavior.apply_green_frontier(
            context,
            self.green.as_mut().expect("issued Green invocation"),
            completion,
        )
    }
    pub(super) fn native<'a, H, O>(
        &'a mut self,
        environment: &'a ResidentEnvironment<H, O>,
        context: &ActorSessionContext,
    ) -> NativeExecution<'a, H, O, A> {
        NativeExecution {
            invocation: self,
            environment,
            context: context.clone(),
        }
    }
    // Ready completions remain in MaybeDone until the original scheduler selects
    // them. Dropping this await never detaches or transfers frontier ownership.
    // Native adapters retain only the boxed operation while polling frontiers;
    // its concrete future stays in one allocation through completion or drop.
    fn drive<'a, T>(
        &'a mut self,
        operation: impl Future<Output = T> + 'a,
    ) -> impl Future<Output = T> + 'a {
        let mut operation = Box::pin(operation);
        async move {
            poll_fn(|cx| {
                if let Some(green) = &mut self.green {
                    for pending in &mut green.pending {
                        let _ = Pin::new(&mut pending.future).poll(cx);
                    }
                }
                operation.as_mut().poll(cx)
            })
            .await
        }
    }
}

/// The invocation's advancing capability. The runner and generic polling
/// implementation are private; consumers submit named native operations.
pub(super) struct NativeExecution<'a, H, O, A> {
    invocation: &'a mut InvocationExecution<A>,
    environment: &'a ResidentEnvironment<H, O>,
    context: ActorSessionContext,
}

impl<H, O, A> NativeExecution<'_, H, O, A>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
    A: Send + 'static,
{
    pub(super) async fn resume_tool_invocation(
        &mut self,
        hole: ResidentHole,
        name: String,
        arguments: serde_json::Value,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.invocation
            .drive(self.environment.runner.resume_tool_invocation(
                self.context.clone(),
                hole,
                name,
                arguments,
            ))
            .await
    }
    pub(super) async fn finish_scope(
        &mut self,
        kernel: &KernelContext,
        frame: scopes::ScopeFrame,
        body: Result<(), tidepool_bridge_effects::ScopeFailure>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.invocation
            .drive(scopes::finish_scope(
                self.environment.clone(),
                kernel.clone(),
                self.context.clone(),
                frame,
                body,
            ))
            .await
    }
    pub(super) async fn capture_boundary(
        &mut self,
        outcome: ResidentOutcome,
        realm: RealmId,
    ) -> Result<ResidentActorBoundary, ResidentActorWorkbenchError> {
        self.invocation
            .drive(
                self.environment
                    .runner
                    .capture_boundary(self.context.clone(), outcome, realm),
            )
            .await
    }
    pub(super) async fn resume_unit(
        &mut self,
        hole: ResidentHole,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.invocation
            .drive(
                self.environment
                    .runner
                    .resume_unit(self.context.clone(), hole),
            )
            .await
    }
    pub(super) async fn resume_reply_rejection(
        &mut self,
        hole: ResidentHole,
        error: crate::ReplyError,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.invocation
            .drive(self.environment.runner.resume_reply_rejection(
                self.context.clone(),
                hole,
                error,
            ))
            .await
    }
    pub(super) async fn resolve_command(
        &mut self,
        behavior: &mut ResidentKernelBehavior<H, O>,
        kernel: &KernelContext,
        hole: ResidentHole,
        request: crate::generated::commands::CommandsReq,
        work: Option<&InvocationWork>,
    ) -> commands::CommandResolution {
        self.invocation
            .drive(behavior.resolve_command(kernel, &self.context, hole, request, work))
            .await
    }
    pub(super) async fn prepare_command_presentation(
        &mut self,
        workbench: &crate::ResidentActorWorkbench<H, O>,
        request: command_presentation::CommandPresentationRequest,
        permitted: bool,
    ) -> Result<command_presentation::PreparedCommandPresentation, ResidentActorWorkbenchError>
    {
        self.invocation
            .drive(command_presentation::prepare(
                &self.environment.commands,
                &self.context,
                workbench,
                request,
                permitted,
            ))
            .await
    }
    pub(super) async fn resume_value<T: tidepool_bridge::ToHaskell + Send + 'static>(
        &mut self,
        hole: ResidentHole,
        value: T,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.invocation
            .drive(
                self.environment
                    .runner
                    .resume_value(self.context.clone(), hole, value),
            )
            .await
    }
    pub(super) async fn run_scope_callback(
        &mut self,
        callback: RootCustody,
        realm: RealmId,
        token: i64,
        work: Arc<InvocationWork>,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        self.invocation
            .drive(self.environment.runner.run_owned_scope_callback(
                self.context.clone(),
                callback,
                realm,
                token,
                work,
            ))
            .await
    }
    pub(super) async fn advance_fragment(
        &mut self,
        workbench: &crate::ResidentActorWorkbench<H, O>,
        request: owned_workbench::WorkbenchFragmentRequest,
    ) -> Result<owned_workbench::WorkbenchFragmentAdvance, ResidentActorWorkbenchError> {
        self.invocation
            .drive(owned_workbench::advance_fragment(
                workbench,
                &self.environment.runner,
                self.context.clone(),
                request,
            ))
            .await
    }
    pub(super) async fn prepare_display(
        &mut self,
        boundary: ResidentActorBoundary,
        allowance: i64,
        operation: Option<WorkbenchOperationId>,
        receipt: Option<DisplayReceiptSubmission<'_>>,
        execution: crate::request::sources::CallbackExecution,
    ) -> Result<(ResidentActorBoundary, Option<WorkbenchDisplayOutput>), ResidentActorWorkbenchError>
    {
        self.invocation
            .drive(prepare_actor_display_boundary(
                self.environment,
                &self.context,
                execution,
                boundary,
                allowance,
                operation,
                receipt,
            ))
            .await
    }
    pub(super) async fn resolve_effect(
        &mut self,
        behavior: &mut ResidentKernelBehavior<H, O>,
        kernel: &KernelContext,
        owner: CurrentEffectOwner<'_>,
        ancestry: &crate::CallAncestry,
        boundary: ResidentActorBoundary,
    ) -> commands::CommandResolution {
        self.invocation
            .drive(behavior.resolve_effect_resolution(
                kernel,
                &self.context,
                owner,
                ancestry,
                boundary,
            ))
            .await
    }
}

impl<A: Send + 'static> GreenInvocation<A> {
    pub(super) fn active_work(&self) -> Option<Arc<InvocationWork>> {
        self.threads
            .get(&self.active)
            .map(|thread| thread.work.clone())
    }

    pub(super) fn active_realm(&self) -> Option<RealmId> {
        self.threads.get(&self.active).map(|thread| thread.realm)
    }

    pub(super) fn active_wait_control(&self) -> Option<Arc<crate::WorkbenchExecutionControl>> {
        self.threads
            .get(&self.active)
            .map(|thread| thread.control.clone())
    }

    fn status(&self, thread: i64) -> Result<ThreadStatus, ResidentActorWorkbenchError> {
        self.threads
            .get(&thread)
            .map(|thread| thread.status)
            .ok_or_else(|| protocol("async handle does not belong to this invocation"))
    }

    fn winner(&self, threads: &[i64]) -> Result<Option<i64>, ResidentActorWorkbenchError> {
        if threads.is_empty() {
            return Err(protocol("async join requires a nonempty list"));
        }
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
        self.pending.push(Pending {
            thread,
            future: maybe_done(Box::pin(async move {
                let outcome = operation.await;
                Completion {
                    thread,
                    scopes,
                    attachment: None,
                    terminal: Vec::new(),
                    result: Some(outcome),
                }
            })),
        });
    }

    pub(super) fn enqueue_frontier(
        &mut self,
        scopes: Vec<scopes::ScopeFrame>,
        future: BoxFuture<'static, (Result<ResidentOutcome, ResidentActorWorkbenchError>, A)>,
    ) {
        let thread = self.active;
        self.pending.push(Pending {
            thread,
            future: maybe_done(Box::pin(async move {
                let (result, attachment) = future.await;
                Completion {
                    thread,
                    scopes,
                    result: Some(result),
                    attachment: Some(attachment),
                    terminal: Vec::new(),
                }
            })),
        });
    }

    pub(super) async fn next(&mut self) -> Result<Completion<A>, ResidentActorWorkbenchError> {
        if self.pending.is_empty() {
            return Err(protocol(
                "async computations have no runnable effect frontier",
            ));
        }
        poll_fn(|cx| {
            for index in 0..self.pending.len() {
                if Pin::new(&mut self.pending[index].future)
                    .poll(cx)
                    .is_ready()
                {
                    let mut pending = self.pending.remove(index);
                    let completion = Pin::new(&mut pending.future)
                        .take_output()
                        .expect("ready frontier owns one completion");
                    return Poll::Ready(Ok(completion));
                }
            }
            Poll::Pending
        })
        .await
    }

    pub(super) fn cancel_parent(&mut self) {
        for thread in self.threads.values() {
            thread.work.close();
        }
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
        self.pending
            .retain(|pending| !descendants.contains(&pending.thread));
        self.joins
            .retain(|join| !descendants.contains(&join.thread));
        descendants
    }
}

impl<H, O> ResidentKernelBehavior<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    pub(super) fn prepare_green_operation<A: Send + 'static>(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        owner: &CurrentEffectOwner<'_>,
        green: &mut GreenInvocation<A>,
        active_scopes: &mut Vec<scopes::ScopeFrame>,
        boundary: GreenBoundary,
    ) -> Result<GreenAdvance, ResidentActorWorkbenchError> {
        let runner = self.environment.runner.clone();
        let resume_context = context.clone();
        match boundary {
            GreenBoundary::Spawn {
                continuation,
                callback,
            } => {
                let (work, realm) = self
                    .register_resource_scope(context, owner)
                    .map_err(|detail| protocol(&detail))?;
                let token = work.scope_token().expect("native child resource identity");
                green.enqueue(
                    std::mem::take(active_scopes),
                    Box::pin(async move {
                        runner
                            .resume_value(resume_context, continuation, token)
                            .await
                    }),
                );
                green.threads.insert(
                    token,
                    Thread {
                        parent: green.active,
                        work: work.clone(),
                        realm,
                        status: ThreadStatus::Running,
                        control: crate::WorkbenchExecutionControl::untracked(),
                    },
                );
                green.active = token;
                return Ok(GreenAdvance::Start {
                    callback,
                    realm,
                    token,
                    work,
                });
            }
            GreenBoundary::Done {
                continuation,
                token,
            } => {
                if green.active == 0 || token != green.active || !active_scopes.is_empty() {
                    return Err(protocol(
                        "async completion does not match its current thread delimiter",
                    ));
                }
                let work = green.active_work().expect("active native thread");
                let thread = green.active;
                let descendants = green.remove_descendant_frontiers(thread);
                let environment = self.environment.clone();
                let kernel = kernel.clone();
                green.pending.push(Pending {
                    thread,
                    future: maybe_done(Box::pin(async move {
                        // Closing the native scope retires the payload-free Done frame.
                        drop(continuation);
                        let closed = close_thread(environment, kernel, work).await;
                        Completion {
                            thread,
                            scopes: Vec::new(),
                            attachment: None,
                            terminal: if closed.is_ok() {
                                descendants
                                    .into_iter()
                                    .map(|id| {
                                        (
                                            id,
                                            if id == thread {
                                                ThreadStatus::Settled
                                            } else {
                                                ThreadStatus::Cancelled
                                            },
                                        )
                                    })
                                    .collect()
                            } else {
                                Vec::new()
                            },
                            result: closed.err().map(Err),
                        }
                    })),
                });
            }
            GreenBoundary::Status {
                continuation,
                thread,
            } => {
                let status = match green.status(thread)? {
                    ThreadStatus::Running => 0_i64,
                    ThreadStatus::Settled => 1,
                    ThreadStatus::Cancelled => 2,
                };
                green.enqueue(
                    std::mem::take(active_scopes),
                    Box::pin(async move {
                        runner
                            .resume_value(resume_context, continuation, status)
                            .await
                    }),
                );
            }
            GreenBoundary::JoinAny {
                continuation,
                threads,
            } => {
                let winner = green.winner(&threads)?;
                let scopes = std::mem::take(active_scopes);
                match winner {
                    Some(winner) => green.enqueue(
                        scopes,
                        Box::pin(async move {
                            runner
                                .resume_value(resume_context, continuation, winner)
                                .await
                        }),
                    ),
                    None => green.joins.push(Join {
                        thread: green.active,
                        continuation,
                        threads,
                        scopes,
                    }),
                }
            }
            GreenBoundary::Cancel {
                continuation,
                thread,
            } => {
                if green.status(thread)? != ThreadStatus::Running {
                    green.enqueue(
                        std::mem::take(active_scopes),
                        Box::pin(
                            async move { runner.resume_unit(resume_context, continuation).await },
                        ),
                    );
                } else {
                    let descendants = green.remove_descendant_frontiers(thread);
                    let work = green.threads[&thread].work.clone();
                    work.close();
                    let caller = green.active;
                    let scopes = std::mem::take(active_scopes);
                    let environment = self.environment.clone();
                    let kernel = kernel.clone();
                    green.pending.push(Pending {
                        thread: caller,
                        future: maybe_done(Box::pin(async move {
                            let closed = close_thread(environment, kernel, work).await;
                            let terminal = if closed.is_ok() {
                                descendants
                                    .into_iter()
                                    .map(|id| (id, ThreadStatus::Cancelled))
                                    .collect()
                            } else {
                                Vec::new()
                            };
                            let result = if caller == thread {
                                drop(continuation);
                                closed.err().map(Err)
                            } else {
                                Some(match closed {
                                    Ok(()) => {
                                        runner.resume_unit(resume_context, continuation).await
                                    }
                                    Err(error) => Err(error),
                                })
                            };
                            Completion {
                                thread: caller,
                                scopes,
                                result,
                                attachment: None,
                                terminal,
                            }
                        })),
                    });
                }
            }
        }
        Ok(GreenAdvance::Wait)
    }

    /// Apply native terminal metadata and wake joins in this invocation.
    pub(super) fn apply_green_frontier<A: Send + 'static>(
        &self,
        context: &ActorSessionContext,
        green: &mut GreenInvocation<A>,
        mut completion: Completion<A>,
    ) -> Result<Option<Completion<A>>, ResidentActorWorkbenchError> {
        if !completion.terminal.is_empty() {
            for (thread, status) in std::mem::take(&mut completion.terminal) {
                green
                    .threads
                    .get_mut(&thread)
                    .expect("issued async thread")
                    .status = status;
            }
            let joins = std::mem::take(&mut green.joins);
            for join in joins {
                match green.winner(&join.threads)? {
                    Some(winner) => {
                        let runner = self.environment.runner.clone();
                        let context = context.clone();
                        let previous = green.active;
                        green.active = join.thread;
                        green.enqueue(
                            join.scopes,
                            Box::pin(async move {
                                runner
                                    .resume_value(context, join.continuation, winner)
                                    .await
                            }),
                        );
                        green.active = previous;
                    }
                    None => green.joins.push(join),
                }
            }
        }
        if completion.result.is_none() {
            return Ok(None);
        }
        green.active = completion.thread;
        Ok(Some(completion))
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
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    work.close();
    let cleanup = work.cleanup(&environment, &kernel).await;
    match cleanup.uncertainty() {
        None => Ok(()),
        Some(detail) => Err(protocol(&format!(
            "async resource cleanup unconfirmed: {detail}"
        ))),
    }
}
