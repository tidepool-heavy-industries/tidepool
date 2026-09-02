//! Canonical sequential Ractor wrapper for Tidepool actor behavior.

use std::collections::HashMap;
use std::marker::PhantomData;
use std::time::Duration;

use futures_util::stream::{FuturesUnordered, StreamExt};
use ractor::{Actor, ActorProcessingErr, ActorRef as RactorRef, SupervisionEvent};

use crate::{
    ActorExitKind, ActorRef, ActorTerminal, ExternalApplicationFailure, ExternalFailureDisposition,
    KernelCallFailure, KernelInvocationFailure, KernelMessage, LocalActorRef, MailboxValue,
    RetainedActorExit,
};
use tidepool_runtime::session::{WorkbenchRequest, WorkbenchResponse};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{detail}")]
pub struct KernelBehaviorError {
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct ChildExitNotice {
    pub child: LocalActorRef,
    pub terminal: ActorTerminal,
}

#[derive(Clone)]
pub struct KernelContext {
    identity: ActorRef,
    myself: RactorRef<KernelMessage>,
    children: std::sync::Arc<parking_lot::Mutex<HashMap<ractor::ActorId, LocalActorRef>>>,
}

impl KernelContext {
    #[must_use]
    pub fn identity(&self) -> ActorRef {
        self.identity
    }

    /// Start a linked child and return its exact handle only after startup.
    pub async fn spawn_child<C>(
        &self,
        name: Option<String>,
        behavior: C,
    ) -> Result<LocalActorRef, ractor::SpawnErr>
    where
        C: KernelBehavior,
    {
        let terminal = RetainedActorExit::new();
        let (address, task) = self
            .myself
            .spawn_linked(
                name,
                LocalActor::<C>(PhantomData),
                LocalActorArguments {
                    behavior,
                    terminal: terminal.clone(),
                },
            )
            .await?;
        drop(task);
        let child = LocalActorRef::new(address, terminal);
        self.children
            .lock()
            .insert(child.address().get_id(), child.clone());
        Ok(child)
    }
}

/// Resident execution owned by one sequential local actor.
///
/// The wrapper owns lifecycle, publication, and reply settlement. A behavior
/// owns Haskell/provider/tool execution and returns domain results without
/// gaining access to Ractor's scheduler internals.
pub trait KernelBehavior: Send + 'static {
    fn start(
        &mut self,
        context: &KernelContext,
    ) -> impl std::future::Future<Output = Result<(), KernelBehaviorError>> + Send;

    fn cast(
        &mut self,
        context: &KernelContext,
        sender: ActorRef,
        request: MailboxValue,
    ) -> impl std::future::Future<Output = Result<(), KernelBehaviorError>> + Send;

    fn call(
        &mut self,
        context: &KernelContext,
        caller: ActorRef,
        ancestry: crate::CallAncestry,
        request: MailboxValue,
    ) -> impl std::future::Future<Output = Result<MailboxValue, KernelBehaviorError>> + Send;

    fn mcp(
        &mut self,
        context: &KernelContext,
        name: String,
        arguments: serde_json::Value,
    ) -> impl std::future::Future<Output = Result<serde_json::Value, KernelInvocationFailure>> + Send;

    fn workbench(
        &mut self,
        context: &KernelContext,
        request: WorkbenchRequest,
    ) -> impl std::future::Future<Output = Result<WorkbenchResponse, KernelInvocationFailure>> + Send;

    fn external_application_failed(
        &mut self,
        context: &KernelContext,
        failure: ExternalApplicationFailure,
    ) -> impl std::future::Future<Output = ExternalFailureDisposition> + Send;

    fn shutdown(
        &mut self,
        context: &KernelContext,
        terminal: &ActorTerminal,
    ) -> impl std::future::Future<Output = Result<(), KernelBehaviorError>> + Send;

    fn child_exited(
        &mut self,
        notice: ChildExitNotice,
    ) -> impl std::future::Future<Output = ()> + Send;
}

pub struct LocalActor<B>(PhantomData<fn() -> B>);

pub struct LocalActorArguments<B> {
    pub behavior: B,
    pub terminal: RetainedActorExit,
}

pub struct LocalActorState<B> {
    context: KernelContext,
    behavior: B,
    terminal: RetainedActorExit,
}

impl<B> Actor for LocalActor<B>
where
    B: KernelBehavior,
{
    type Msg = KernelMessage;
    type State = LocalActorState<B>;
    type Arguments = LocalActorArguments<B>;

    async fn pre_start(
        &self,
        myself: RactorRef<Self::Msg>,
        arguments: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        let identity = ActorRef::first(crate::ActorId(myself.get_id().pid()));
        let context = KernelContext {
            identity,
            myself,
            children: std::sync::Arc::new(parking_lot::Mutex::new(HashMap::new())),
        };
        let mut state = LocalActorState {
            context,
            behavior: arguments.behavior,
            terminal: arguments.terminal,
        };
        if let Err(error) = state.behavior.start(&state.context).await {
            let terminal = failed_terminal(format!("actor startup failed: {error}"));
            let _ = state.terminal.publish(terminal);
            return Err(Box::new(error));
        }
        Ok(state)
    }

    async fn handle(
        &self,
        myself: RactorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            KernelMessage::Cast { sender, request } => {
                if let Err(error) = state.behavior.cast(&state.context, sender, request).await {
                    fail_actor(&myself, state, format!("actor cast failed: {error}"));
                }
            }
            KernelMessage::Call {
                caller,
                ancestry,
                request,
                reply,
            } => match ancestry.enter(state.context.identity) {
                Err(error) => {
                    let _ = reply.send(Err(error));
                }
                Ok(ancestry) => match state
                    .behavior
                    .call(&state.context, caller, ancestry, request)
                    .await
                {
                    Ok(value) => {
                        let _ = reply.send(Ok(value));
                    }
                    Err(error) => {
                        let detail = error.to_string();
                        let _ = reply.send(Err(KernelCallFailure::Handler {
                            actor: state.context.identity,
                            detail: detail.clone(),
                        }));
                        fail_actor(&myself, state, format!("actor call failed: {detail}"));
                    }
                },
            },
            KernelMessage::Mcp {
                name,
                arguments,
                reply,
            } => {
                let _ = reply.send(state.behavior.mcp(&state.context, name, arguments).await);
            }
            KernelMessage::Workbench { request, reply } => {
                let _ = reply.send(state.behavior.workbench(&state.context, request).await);
            }
            KernelMessage::ExternalApplicationFailed { failure, reply } => {
                let detail = format!("native actor application failed: {}", failure.detail);
                let disposition = state
                    .behavior
                    .external_application_failed(&state.context, failure)
                    .await;
                let _ = reply.send(disposition);
                if disposition == ExternalFailureDisposition::Applied {
                    fail_actor(&myself, state, detail);
                }
            }
            KernelMessage::Shutdown { terminal, reply } => {
                shutdown_children(&state.context, Duration::from_secs(15)).await;
                let terminal = match state.behavior.shutdown(&state.context, &terminal).await {
                    Ok(()) => terminal,
                    Err(error) => failed_terminal(format!("actor shutdown failed: {error}")),
                };
                publish_terminal(&state.terminal, &terminal);
                let _ = reply.send(terminal.clone());
                myself.stop(Some(terminal.summary.clone()));
            }
        }
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        _myself: RactorRef<Self::Msg>,
        event: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let (cell, observed) = match event {
            SupervisionEvent::ActorStarted(_) | SupervisionEvent::ProcessGroupChanged(_) => {
                return Ok(())
            }
            SupervisionEvent::ActorTerminated(cell, _, reason) => {
                let summary = reason.unwrap_or_else(|| {
                    "linked child stopped without publishing a terminal result".into()
                });
                (cell, failed_terminal(summary))
            }
            SupervisionEvent::ActorFailed(cell, error) => (
                cell,
                failed_terminal(format!("linked child actor failed: {error}")),
            ),
        };
        let child = state.context.children.lock().get(&cell.get_id()).cloned();
        let Some(child) = child else {
            tracing::warn!(child = %cell.get_id(), "received lifecycle event for an unregistered linked child");
            return Ok(());
        };
        if child.terminal().get().is_none() {
            publish_terminal(child.terminal(), &observed);
        }
        let Some(terminal) = child.terminal().get() else {
            unreachable!("supervision publishes or observes the child terminal result");
        };
        state
            .behavior
            .child_exited(ChildExitNotice { child, terminal })
            .await;
        Ok(())
    }
}

/// Spawn one root through the same actor implementation used for children.
pub async fn spawn_local_actor<B>(
    name: Option<String>,
    behavior: B,
) -> Result<(LocalActorRef, ractor::concurrency::JoinHandle<()>), ractor::SpawnErr>
where
    B: KernelBehavior,
{
    let terminal = RetainedActorExit::new();
    let (address, task) = Actor::spawn(
        name,
        LocalActor::<B>(PhantomData),
        LocalActorArguments {
            behavior,
            terminal: terminal.clone(),
        },
    )
    .await?;
    Ok((LocalActorRef::new(address, terminal), task))
}

fn fail_actor<B>(myself: &RactorRef<KernelMessage>, state: &LocalActorState<B>, detail: String) {
    let terminal = failed_terminal(detail);
    publish_terminal(&state.terminal, &terminal);
    myself.stop(Some(terminal.summary));
}

fn publish_terminal(retained: &RetainedActorExit, terminal: &ActorTerminal) {
    if let Err(error) = retained.publish(terminal.clone()) {
        tracing::error!(?terminal, existing = ?error.existing, "actor terminal result published twice");
    }
}

fn failed_terminal(summary: String) -> ActorTerminal {
    ActorTerminal {
        kind: ActorExitKind::Failed,
        summary,
    }
}

async fn shutdown_children(context: &KernelContext, timeout: Duration) {
    let children: Vec<_> = context.children.lock().values().cloned().collect();
    let mut shutdowns = FuturesUnordered::new();
    for child in children {
        shutdowns.push(async move {
            if child.terminal().get().is_some() {
                return;
            }
            let requested = ActorTerminal {
                kind: ActorExitKind::Cancelled,
                summary: "owner actor stopped".into(),
            };
            let result = child
                .address()
                .call(
                    |reply| KernelMessage::Shutdown {
                        terminal: requested.clone(),
                        reply,
                    },
                    Some(timeout),
                )
                .await;
            if !matches!(result, Ok(ractor::rpc::CallResult::Success(_))) {
                if child.terminal().get().is_none() {
                    publish_terminal(child.terminal(), &requested);
                }
                child.address().kill();
            }
        });
    }
    while shutdowns.next().await.is_some() {}
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use parking_lot::Mutex;
    use tidepool_repr::SessionId;
    use tidepool_runtime::session::{WorkbenchResponse, WorkbenchRunStatus};
    use tokio::sync::{oneshot, Notify};

    use super::*;

    struct ProbeBehavior {
        calls: Arc<Mutex<Vec<&'static str>>>,
        release_first: Arc<Notify>,
        fail_cast: bool,
        spawned_child: Arc<Mutex<Option<LocalActorRef>>>,
        child_exits: Arc<Mutex<Vec<ActorTerminal>>>,
    }

    impl KernelBehavior for ProbeBehavior {
        async fn start(&mut self, _context: &KernelContext) -> Result<(), KernelBehaviorError> {
            Ok(())
        }

        async fn cast(
            &mut self,
            _context: &KernelContext,
            _sender: ActorRef,
            _request: MailboxValue,
        ) -> Result<(), KernelBehaviorError> {
            if self.fail_cast {
                Err(KernelBehaviorError {
                    detail: "cast probe".into(),
                })
            } else {
                Ok(())
            }
        }

        async fn call(
            &mut self,
            _context: &KernelContext,
            _caller: ActorRef,
            _ancestry: crate::CallAncestry,
            request: MailboxValue,
        ) -> Result<MailboxValue, KernelBehaviorError> {
            Ok(request)
        }

        async fn mcp(
            &mut self,
            context: &KernelContext,
            name: String,
            _arguments: serde_json::Value,
        ) -> Result<serde_json::Value, KernelInvocationFailure> {
            if name == "spawn" {
                let child = context
                    .spawn_child(None, FailingChild)
                    .await
                    .map_err(|error| KernelInvocationFailure::Failed {
                        actor: context.identity(),
                        detail: error.to_string(),
                    })?;
                *self.spawned_child.lock() = Some(child);
            } else if name == "first" {
                self.calls.lock().push("first-start");
                self.release_first.notified().await;
                self.calls.lock().push("first-end");
            } else {
                self.calls.lock().push("second");
            }
            Ok(serde_json::Value::String(name))
        }

        async fn workbench(
            &mut self,
            _context: &KernelContext,
            _request: WorkbenchRequest,
        ) -> Result<WorkbenchResponse, KernelInvocationFailure> {
            Ok(WorkbenchResponse {
                status: WorkbenchRunStatus::Committed,
                items: Vec::new(),
                next_index: 0,
                total: 0,
            })
        }

        async fn external_application_failed(
            &mut self,
            _context: &KernelContext,
            _failure: ExternalApplicationFailure,
        ) -> ExternalFailureDisposition {
            ExternalFailureDisposition::Applied
        }

        async fn shutdown(
            &mut self,
            _context: &KernelContext,
            _terminal: &ActorTerminal,
        ) -> Result<(), KernelBehaviorError> {
            Ok(())
        }

        async fn child_exited(&mut self, notice: ChildExitNotice) {
            self.child_exits.lock().push(notice.terminal);
        }
    }

    struct FailingChild;

    impl KernelBehavior for FailingChild {
        async fn start(&mut self, _context: &KernelContext) -> Result<(), KernelBehaviorError> {
            Ok(())
        }

        async fn cast(
            &mut self,
            _context: &KernelContext,
            _sender: ActorRef,
            _request: MailboxValue,
        ) -> Result<(), KernelBehaviorError> {
            Err(KernelBehaviorError {
                detail: "child failed".into(),
            })
        }

        async fn call(
            &mut self,
            _context: &KernelContext,
            _caller: ActorRef,
            _ancestry: crate::CallAncestry,
            request: MailboxValue,
        ) -> Result<MailboxValue, KernelBehaviorError> {
            Ok(request)
        }

        async fn mcp(
            &mut self,
            context: &KernelContext,
            _name: String,
            _arguments: serde_json::Value,
        ) -> Result<serde_json::Value, KernelInvocationFailure> {
            Err(KernelInvocationFailure::Rejected {
                actor: context.identity(),
                detail: "child has no MCP policy".into(),
            })
        }

        async fn workbench(
            &mut self,
            context: &KernelContext,
            _request: WorkbenchRequest,
        ) -> Result<WorkbenchResponse, KernelInvocationFailure> {
            Err(KernelInvocationFailure::Rejected {
                actor: context.identity(),
                detail: "child has no workbench".into(),
            })
        }

        async fn external_application_failed(
            &mut self,
            _context: &KernelContext,
            _failure: ExternalApplicationFailure,
        ) -> ExternalFailureDisposition {
            ExternalFailureDisposition::Applied
        }

        async fn shutdown(
            &mut self,
            _context: &KernelContext,
            _terminal: &ActorTerminal,
        ) -> Result<(), KernelBehaviorError> {
            Ok(())
        }

        async fn child_exited(&mut self, _notice: ChildExitNotice) {}
    }

    fn behavior(
        fail_cast: bool,
    ) -> (
        ProbeBehavior,
        Arc<Mutex<Vec<&'static str>>>,
        Arc<Notify>,
        Arc<Mutex<Option<LocalActorRef>>>,
        Arc<Mutex<Vec<ActorTerminal>>>,
    ) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let release = Arc::new(Notify::new());
        let spawned_child = Arc::new(Mutex::new(None));
        let child_exits = Arc::new(Mutex::new(Vec::new()));
        (
            ProbeBehavior {
                calls: Arc::clone(&calls),
                release_first: Arc::clone(&release),
                fail_cast,
                spawned_child: Arc::clone(&spawned_child),
                child_exits: Arc::clone(&child_exits),
            },
            calls,
            release,
            spawned_child,
            child_exits,
        )
    }

    #[tokio::test]
    async fn one_actor_never_reenters_while_an_operation_is_pending() {
        let (behavior, calls, release, _, _) = behavior(false);
        let (actor, task) = spawn_local_actor(None, behavior).await.expect("spawn");
        let (first_tx, first_rx) = oneshot::channel();
        let (second_tx, second_rx) = oneshot::channel();
        actor
            .address()
            .send_message(KernelMessage::Mcp {
                name: "first".into(),
                arguments: serde_json::Value::Null,
                reply: first_tx.into(),
            })
            .expect("queue first");
        actor
            .address()
            .send_message(KernelMessage::Mcp {
                name: "second".into(),
                arguments: serde_json::Value::Null,
                reply: second_tx.into(),
            })
            .expect("queue second");

        tokio::task::yield_now().await;
        assert_eq!(&*calls.lock(), &["first-start"]);
        release.notify_one();
        assert_eq!(first_rx.await.expect("first reply").unwrap(), "first");
        assert_eq!(second_rx.await.expect("second reply").unwrap(), "second");
        assert_eq!(&*calls.lock(), &["first-start", "first-end", "second"]);

        let terminal = ActorTerminal {
            kind: ActorExitKind::Completed,
            summary: "done".into(),
        };
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        actor
            .address()
            .send_message(KernelMessage::Shutdown {
                terminal: terminal.clone(),
                reply: shutdown_tx.into(),
            })
            .expect("queue shutdown");
        assert_eq!(shutdown_rx.await.expect("shutdown reply"), terminal);
        task.await.expect("actor task");
        assert_eq!(actor.terminal().wait().await, terminal);
    }

    #[tokio::test]
    async fn behavior_failure_publishes_once_and_releases_message_custody() {
        let (behavior, _, _, _, _) = behavior(true);
        let (actor, task) = spawn_local_actor(None, behavior).await.expect("spawn");
        let dropped = Arc::new(AtomicUsize::new(0));
        actor
            .address()
            .send_message(KernelMessage::Cast {
                sender: ActorRef::first(crate::ActorId(99)),
                request: MailboxValue::probe(SessionId(1), Arc::clone(&dropped)),
            })
            .expect("queue cast");

        let terminal = actor.terminal().wait().await;
        task.await.expect("actor task");
        assert_eq!(terminal.kind, ActorExitKind::Failed);
        assert!(terminal.summary.contains("cast probe"));
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn child_failure_is_retained_and_notifies_without_killing_owner() {
        let (behavior, _, _, child_slot, child_exits) = behavior(false);
        let (owner, owner_task) = spawn_local_actor(None, behavior)
            .await
            .expect("spawn owner");
        let (spawn_tx, spawn_rx) = oneshot::channel();
        owner
            .address()
            .send_message(KernelMessage::Mcp {
                name: "spawn".into(),
                arguments: serde_json::Value::Null,
                reply: spawn_tx.into(),
            })
            .expect("request child");
        spawn_rx.await.expect("spawn reply").expect("spawn result");
        let child = child_slot.lock().clone().expect("child handle");
        child
            .address()
            .send_message(KernelMessage::Cast {
                sender: owner.identity(),
                request: MailboxValue::probe(SessionId(1), Arc::new(AtomicUsize::new(0))),
            })
            .expect("fail child");

        assert_eq!(child.terminal().wait().await.kind, ActorExitKind::Failed);
        for _ in 0..20 {
            if !child_exits.lock().is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(child_exits.lock().len(), 1);

        let (ping_tx, ping_rx) = oneshot::channel();
        owner
            .address()
            .send_message(KernelMessage::Mcp {
                name: "second".into(),
                arguments: serde_json::Value::Null,
                reply: ping_tx.into(),
            })
            .expect("owner remains callable");
        assert_eq!(ping_rx.await.expect("owner reply").unwrap(), "second");

        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        owner
            .address()
            .send_message(KernelMessage::Shutdown {
                terminal: ActorTerminal {
                    kind: ActorExitKind::Completed,
                    summary: "owner done".into(),
                },
                reply: shutdown_tx.into(),
            })
            .expect("shutdown owner");
        shutdown_rx.await.expect("shutdown reply");
        owner_task.await.expect("owner task");
    }
}
