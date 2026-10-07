//! Invocation-local frontiers over the existing actor execution task. Native
//! continuations and Haskell result cells remain owned by the resident machine.

use super::*;
use futures_util::future::{poll_fn, BoxFuture};
use std::collections::BTreeMap;
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
    future: BoxFuture<'static, Completion<A>>,
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
            future: Box::pin(async move {
                let outcome = operation.await;
                Completion {
                    thread,
                    scopes,
                    attachment: None,
                    terminal: Vec::new(),
                    result: Some(outcome),
                }
            }),
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
            future: Box::pin(async move {
                let (result, attachment) = future.await;
                Completion {
                    thread,
                    scopes,
                    result: Some(result),
                    attachment: Some(attachment),
                    terminal: Vec::new(),
                }
            }),
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
                if let Poll::Ready(completion) = self.pending[index].future.as_mut().poll(cx) {
                    drop(self.pending.remove(index));
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
                    future: Box::pin(async move {
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
                    }),
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
                        future: Box::pin(async move {
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
                        }),
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
