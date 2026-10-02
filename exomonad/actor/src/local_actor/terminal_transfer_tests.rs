use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use tidepool_runtime::session::{WorkbenchRequest, WorkbenchRunStatus};
use tokio::sync::{oneshot, Semaphore};

#[derive(Clone, Copy)]
enum FirstFailure {
    TerminalTransfer,
    Ordinary,
}

struct CompletionProbe {
    failure: FirstFailure,
    completions: usize,
    cleanup_calls: Arc<AtomicUsize>,
    cleanup_entered: Option<oneshot::Sender<()>>,
    cleanup_release: Arc<Semaphore>,
}

impl KernelBehavior for CompletionProbe {
    fn start<'a>(
        &'a mut self,
        _context: &'a KernelContext,
    ) -> BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>> {
        Box::pin(async { Ok(KernelStep::Continue(())) })
    }

    fn cast<'a>(
        &'a mut self,
        _context: &'a KernelContext,
        _sender: ActorRef,
        _request: MailboxValue,
    ) -> BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>> {
        Box::pin(async { panic!("completion probe has no casts") })
    }

    fn call<'a>(
        &'a mut self,
        _context: &'a KernelContext,
        _caller: ActorRef,
        _ancestry: crate::CallAncestry,
        _request: MailboxValue,
    ) -> BoxFuture<'a, Result<KernelStep<MailboxValue>, KernelBehaviorError>> {
        Box::pin(async { panic!("completion probe has no calls") })
    }

    fn tool<'a>(
        &'a mut self,
        _context: &'a KernelContext,
        _invocation: exomonad_tool::ToolInvocation,
        _capture: Option<Arc<dyn crate::HostedCheckpointCapture>>,
    ) -> BoxFuture<'a, Result<KernelStep<serde_json::Value>, KernelInvocationFailure>> {
        Box::pin(async { panic!("completion probe has no tools") })
    }

    fn workbench<'a>(
        &'a mut self,
        _context: &'a KernelContext,
        _invocation: crate::ActorWorkbenchInvocation,
        _control: Option<Arc<crate::WorkbenchExecutionControl>>,
    ) -> BoxFuture<'a, Result<KernelStep<WorkbenchResponse>, KernelInvocationFailure>> {
        Box::pin(async { panic!("completion probe requires owned dispatch") })
    }

    fn dispatch_workbench(
        &mut self,
        context: &KernelContext,
        _invocation: crate::ActorWorkbenchInvocation,
        _control: Option<Arc<crate::WorkbenchExecutionControl>>,
    ) -> WorkbenchDispatch<Self> {
        let actor = context.identity();
        WorkbenchDispatch::Owned(OwnedWorkbenchTask::new(Box::pin(async move {
            OwnedWorkbenchCompletion::new(move |behavior: &mut Self| {
                behavior.completions += 1;
                if behavior.completions == 1 {
                    let source = KernelInvocationFailure::CleanupUnconfirmed {
                        actor,
                        detail: "controlled finalization refusal".into(),
                    };
                    return Err(match behavior.failure {
                        FirstFailure::TerminalTransfer => {
                            KernelInvocationFailure::TerminalTransferFailed {
                                actor,
                                request: crate::RequestId(19),
                                source: Box::new(source),
                            }
                        }
                        FirstFailure::Ordinary => source,
                    });
                }
                Ok(KernelStep::Continue(WorkbenchResponse {
                    status: WorkbenchRunStatus::Committed,
                    summary: None,
                    items: Vec::new(),
                    next_index: 0,
                    total: 0,
                }))
            })
        })))
    }

    fn external_application_failed<'a>(
        &'a mut self,
        _context: &'a KernelContext,
        _failure: ExternalApplicationFailure,
    ) -> BoxFuture<'a, ExternalFailureDisposition> {
        Box::pin(async { ExternalFailureDisposition::Applied })
    }

    fn shutdown<'a>(
        &'a mut self,
        _context: &'a KernelContext,
        _terminal: &'a ActorTerminal,
    ) -> BoxFuture<'a, Result<(), KernelBehaviorError>> {
        Box::pin(async { panic!("completion probe uses explicit cleanup components") })
    }

    fn shutdown_components<'a>(
        &'a mut self,
        _context: &'a KernelContext,
        _terminal: &'a ActorTerminal,
        _deadline: tokio::time::Instant,
    ) -> BoxFuture<
        'a,
        (
            crate::CleanupComponentOutcome,
            crate::CleanupComponentOutcome,
        ),
    > {
        self.cleanup_calls.fetch_add(1, Ordering::SeqCst);
        let entered = self.cleanup_entered.take();
        let release = self.cleanup_release.clone();
        Box::pin(async move {
            if let Some(entered) = entered {
                entered.send(()).expect("cleanup observer retained");
            }
            release
                .acquire()
                .await
                .expect("cleanup gate retained")
                .forget();
            // This extractor-free behavior owns no native realm or resources.
            (
                crate::CleanupComponentOutcome::Confirmed,
                crate::CleanupComponentOutcome::Confirmed,
            )
        })
    }

    fn stopped<'a>(
        &'a mut self,
        _context: &'a KernelContext,
        _terminal: &'a ActorTerminal,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn child_exited(&mut self, _notice: ChildExitNotice) {}
}

fn send_workbench(actor: &LocalActorRef) -> oneshot::Receiver<crate::KernelWorkbenchReply> {
    let (reply, receive) = oneshot::channel();
    actor
        .address()
        .send_message(KernelMessage::Workbench {
            invocation: crate::ActorWorkbenchInvocation::unbound(
                WorkbenchRequest::from_cell_input("pure ()"),
            ),
            control: None,
            reply: reply.into(),
        })
        .expect("admit workbench request");
    receive
}

#[tokio::test]
async fn owned_terminal_transfer_failure_settles_caller_before_failed_retirement() {
    let cleanup_calls = Arc::new(AtomicUsize::new(0));
    let cleanup_release = Arc::new(Semaphore::new(0));
    let (entered, receive_entered) = oneshot::channel();
    let (actor, task) = spawn_local_actor(
        None,
        CompletionProbe {
            failure: FirstFailure::TerminalTransfer,
            completions: 0,
            cleanup_calls: cleanup_calls.clone(),
            cleanup_entered: Some(entered),
            cleanup_release: cleanup_release.clone(),
        },
    )
    .await
    .expect("spawn actor");
    let mut reply = send_workbench(&actor);
    tokio::time::timeout(Duration::from_secs(5), receive_entered)
        .await
        .expect("failed retirement entered cleanup")
        .expect("cleanup event delivered");
    let failure = reply
        .try_recv()
        .expect("caller settled before cleanup began")
        .expect_err("terminal transfer returns its original caller failure");
    assert!(
        matches!(failure, KernelInvocationFailure::TerminalTransferFailed {
        actor: failed_actor,
        request: crate::RequestId(19),
        source,
    } if failed_actor == actor.identity()
        && matches!(*source, KernelInvocationFailure::CleanupUnconfirmed { actor: source_actor, .. }
            if source_actor == actor.identity()))
    );
    assert!(
        actor.terminal().get().is_none(),
        "exit waits for cleanup confirmation"
    );
    assert_eq!(cleanup_calls.load(Ordering::SeqCst), 1);
    cleanup_release.add_permits(1);
    let terminal = tokio::time::timeout(Duration::from_secs(5), actor.terminal().wait())
        .await
        .expect("retained failed exit");
    assert_eq!(terminal.kind, ActorExitKind::Failed);
    assert!(actor
        .terminal()
        .cleanup()
        .expect("retained cleanup receipt")
        .is_confirmed());
    task.await.expect("actor task closed");
    assert_eq!(
        actor
            .shutdown(terminal.clone())
            .await
            .expect("repeat retirement reads retained exit"),
        terminal
    );
    assert_eq!(cleanup_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn ordinary_owned_completion_failure_keeps_actor_reusable() {
    let cleanup_calls = Arc::new(AtomicUsize::new(0));
    let (actor, task) = spawn_local_actor(
        None,
        CompletionProbe {
            failure: FirstFailure::Ordinary,
            completions: 0,
            cleanup_calls: cleanup_calls.clone(),
            cleanup_entered: None,
            cleanup_release: Arc::new(Semaphore::new(1)),
        },
    )
    .await
    .expect("spawn actor");
    let first = tokio::time::timeout(Duration::from_secs(5), send_workbench(&actor))
        .await
        .expect("ordinary failure returned")
        .expect("reply delivered");
    assert!(
        matches!(first, Err(KernelInvocationFailure::CleanupUnconfirmed { actor: failed_actor, .. })
        if failed_actor == actor.identity())
    );
    let second = tokio::time::timeout(Duration::from_secs(5), send_workbench(&actor))
        .await
        .expect("next request returned")
        .expect("reply delivered")
        .expect("same actor remains usable");
    assert_eq!(second.status, WorkbenchRunStatus::Committed);
    assert!(actor.terminal().get().is_none());
    assert_eq!(cleanup_calls.load(Ordering::SeqCst), 0);
    actor
        .shutdown(ActorTerminal {
            kind: ActorExitKind::Completed,
            summary: "control complete".into(),
        })
        .await
        .expect("retire control actor");
    task.await.expect("actor task closed");
    assert_eq!(cleanup_calls.load(Ordering::SeqCst), 1);
}
