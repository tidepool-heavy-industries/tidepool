//! Canonical sequential Ractor wrapper for Tidepool actor behavior.

use std::marker::PhantomData;

use ractor::{Actor, ActorProcessingErr, ActorRef as RactorRef};

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

/// Resident execution owned by one sequential local actor.
///
/// The wrapper owns lifecycle, publication, and reply settlement. A behavior
/// owns Haskell/provider/tool execution and returns domain results without
/// gaining access to Ractor's scheduler internals.
pub trait KernelBehavior: Send + 'static {
    fn start(
        &mut self,
        actor: ActorRef,
    ) -> impl std::future::Future<Output = Result<(), KernelBehaviorError>> + Send;

    fn cast(
        &mut self,
        sender: ActorRef,
        request: MailboxValue,
    ) -> impl std::future::Future<Output = Result<(), KernelBehaviorError>> + Send;

    fn call(
        &mut self,
        caller: ActorRef,
        ancestry: crate::CallAncestry,
        request: MailboxValue,
    ) -> impl std::future::Future<Output = Result<MailboxValue, KernelBehaviorError>> + Send;

    fn mcp(
        &mut self,
        name: String,
        arguments: serde_json::Value,
    ) -> impl std::future::Future<Output = Result<serde_json::Value, KernelInvocationFailure>> + Send;

    fn workbench(
        &mut self,
        request: WorkbenchRequest,
    ) -> impl std::future::Future<Output = Result<WorkbenchResponse, KernelInvocationFailure>> + Send;

    fn external_application_failed(
        &mut self,
        failure: ExternalApplicationFailure,
    ) -> impl std::future::Future<Output = ExternalFailureDisposition> + Send;

    fn shutdown(
        &mut self,
        terminal: &ActorTerminal,
    ) -> impl std::future::Future<Output = Result<(), KernelBehaviorError>> + Send;
}

pub struct LocalActor<B>(PhantomData<fn() -> B>);

pub struct LocalActorArguments<B> {
    pub behavior: B,
    pub terminal: RetainedActorExit,
}

pub struct LocalActorState<B> {
    identity: ActorRef,
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
        let mut state = LocalActorState {
            identity,
            behavior: arguments.behavior,
            terminal: arguments.terminal,
        };
        if let Err(error) = state.behavior.start(identity).await {
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
                if let Err(error) = state.behavior.cast(sender, request).await {
                    fail_actor(&myself, state, format!("actor cast failed: {error}"));
                }
            }
            KernelMessage::Call {
                caller,
                ancestry,
                request,
                reply,
            } => match ancestry.enter(state.identity) {
                Err(error) => {
                    let _ = reply.send(Err(error));
                }
                Ok(ancestry) => match state.behavior.call(caller, ancestry, request).await {
                    Ok(value) => {
                        let _ = reply.send(Ok(value));
                    }
                    Err(error) => {
                        let detail = error.to_string();
                        let _ = reply.send(Err(KernelCallFailure::Handler {
                            actor: state.identity,
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
                let _ = reply.send(state.behavior.mcp(name, arguments).await);
            }
            KernelMessage::Workbench { request, reply } => {
                let _ = reply.send(state.behavior.workbench(request).await);
            }
            KernelMessage::ExternalApplicationFailed { failure, reply } => {
                let detail = format!("native actor application failed: {}", failure.detail);
                let disposition = state.behavior.external_application_failed(failure).await;
                let _ = reply.send(disposition);
                if disposition == ExternalFailureDisposition::Applied {
                    fail_actor(&myself, state, detail);
                }
            }
            KernelMessage::Shutdown { terminal, reply } => {
                let terminal = match state.behavior.shutdown(&terminal).await {
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
    }

    impl KernelBehavior for ProbeBehavior {
        async fn start(&mut self, _actor: ActorRef) -> Result<(), KernelBehaviorError> {
            Ok(())
        }

        async fn cast(
            &mut self,
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
            _caller: ActorRef,
            _ancestry: crate::CallAncestry,
            request: MailboxValue,
        ) -> Result<MailboxValue, KernelBehaviorError> {
            Ok(request)
        }

        async fn mcp(
            &mut self,
            name: String,
            _arguments: serde_json::Value,
        ) -> Result<serde_json::Value, KernelInvocationFailure> {
            if name == "first" {
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
            _failure: ExternalApplicationFailure,
        ) -> ExternalFailureDisposition {
            ExternalFailureDisposition::Applied
        }

        async fn shutdown(&mut self, _terminal: &ActorTerminal) -> Result<(), KernelBehaviorError> {
            Ok(())
        }
    }

    fn behavior(fail_cast: bool) -> (ProbeBehavior, Arc<Mutex<Vec<&'static str>>>, Arc<Notify>) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let release = Arc::new(Notify::new());
        (
            ProbeBehavior {
                calls: Arc::clone(&calls),
                release_first: Arc::clone(&release),
                fail_cast,
            },
            calls,
            release,
        )
    }

    #[tokio::test]
    async fn one_actor_never_reenters_while_an_operation_is_pending() {
        let (behavior, calls, release) = behavior(false);
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
        let (behavior, _, _) = behavior(true);
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
}
