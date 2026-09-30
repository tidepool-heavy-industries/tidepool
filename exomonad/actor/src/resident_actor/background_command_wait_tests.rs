use super::*;
use crate::command_jobs::{CommandBackend, CommandControl};
use crate::{
    ActorExitKind, ActorTerminal, ChildExitNotice, ExternalApplicationFailure,
    ExternalFailureDisposition, KernelBehavior, KernelBehaviorError, KernelContext,
    KernelInvocationFailure, KernelStep, LocalActorRef, MailboxValue,
};
use futures_util::future::BoxFuture;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use tidepool_bridge_effects::{
    CommandCleanup, CommandInput, CommandOutcome, CommandOutput, CommandPosition, CommandResult,
    CommandSpec, CommandStatus,
};

struct Owner(Option<tokio::sync::oneshot::Sender<KernelContext>>);

impl KernelBehavior for Owner {
    fn start<'a>(
        &'a mut self,
        context: &'a KernelContext,
    ) -> BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>> {
        assert!(self.0.take().unwrap().send(context.clone()).is_ok());
        Box::pin(async { Ok(KernelStep::Continue(())) })
    }

    fn cast<'a>(
        &'a mut self,
        _: &'a KernelContext,
        _: ActorRef,
        _: MailboxValue,
    ) -> BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>> {
        Box::pin(async { panic!("fixture has no mailbox casts") })
    }

    fn call<'a>(
        &'a mut self,
        _: &'a KernelContext,
        _: ActorRef,
        _: crate::CallAncestry,
        _: MailboxValue,
    ) -> BoxFuture<'a, Result<KernelStep<MailboxValue>, KernelBehaviorError>> {
        Box::pin(async { panic!("fixture has no mailbox calls") })
    }

    fn tool<'a>(
        &'a mut self,
        _: &'a KernelContext,
        _: exomonad_tool::ToolInvocation,
        _: Option<Arc<dyn crate::HostedCheckpointCapture>>,
    ) -> BoxFuture<'a, Result<KernelStep<serde_json::Value>, KernelInvocationFailure>> {
        Box::pin(async { panic!("fixture has no tools") })
    }

    fn workbench<'a>(
        &'a mut self,
        _: &'a KernelContext,
        _: crate::ActorWorkbenchInvocation,
        _: Option<Arc<crate::WorkbenchExecutionControl>>,
    ) -> BoxFuture<
        'a,
        Result<KernelStep<tidepool_runtime::session::WorkbenchResponse>, KernelInvocationFailure>,
    > {
        Box::pin(async { panic!("fixture has no workbench") })
    }

    fn external_application_failed<'a>(
        &'a mut self,
        _: &'a KernelContext,
        _: ExternalApplicationFailure,
    ) -> BoxFuture<'a, ExternalFailureDisposition> {
        Box::pin(async { panic!("fixture has no external application") })
    }

    fn shutdown<'a>(
        &'a mut self,
        _: &'a KernelContext,
        _: &'a ActorTerminal,
    ) -> BoxFuture<'a, Result<(), KernelBehaviorError>> {
        Box::pin(async { Ok(()) })
    }

    fn stopped<'a>(&'a mut self, _: &'a KernelContext, _: &'a ActorTerminal) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn child_exited(&mut self, _: ChildExitNotice) {}
}

#[derive(Default)]
struct Backend {
    reads: AtomicUsize,
    unavailable: AtomicBool,
}

impl CommandBackend for Backend {
    fn execute<'a>(
        &'a self,
        _: &'a str,
        _: CommandSpec,
        _: tokio::sync::watch::Sender<CommandStatus>,
    ) -> BoxFuture<'a, CommandResult> {
        Box::pin(async {
            CommandResult {
                outcome: CommandOutcome::CommandExited(0),
                cleanup: CommandCleanup::CommandClean,
            }
        })
    }

    fn control<'a>(
        &'a self,
        _: &'a str,
        _: CommandControl,
    ) -> BoxFuture<'a, Result<(), CommandError>> {
        Box::pin(async { Ok(()) })
    }

    fn output<'a>(
        &'a self,
        _: &'a str,
        _: usize,
    ) -> BoxFuture<'a, Result<CommandOutput, CommandError>> {
        Box::pin(async { panic!("observation uses explicit read pages") })
    }

    fn read<'a>(
        &'a self,
        _: &'a str,
        stream: CommandStream,
        position: CommandPosition,
    ) -> BoxFuture<'a, Result<CommandPage, CommandError>> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        Box::pin(async move {
            if self.unavailable.load(Ordering::Relaxed) {
                return Err(CommandError::CommandUnavailable(
                    "fixture read unavailable".into(),
                ));
            }
            let text = match stream {
                CommandStream::Stdout => "retained stdout\n",
                CommandStream::Stderr => "retained stderr\n",
            };
            let start = match position {
                CommandPosition::OutputOffset(start) => start.min(text.len() as i64),
                CommandPosition::OutputBeginning => 0,
                _ => panic!("unexpected fixture page request"),
            };
            Ok(CommandPage {
                text: text[start as usize..].to_owned(),
                start,
                end: text.len() as i64,
                available_end: text.len() as i64,
                retained_start: 0,
                lost_bytes: 0,
                finished: true,
                lossy: false,
                leading_fragment: false,
                trailing_fragment: false,
            })
        })
    }

    fn cleanup<'a>(&'a self, _: &'a str) -> BoxFuture<'a, CommandCleanup> {
        Box::pin(async { CommandCleanup::CommandClean })
    }
}

struct Fixture {
    jobs: CommandJobs,
    actor: LocalActorRef,
    task: ractor::concurrency::JoinHandle<()>,
    backend: Arc<Backend>,
    job: String,
}

impl Fixture {
    async fn start() -> Self {
        let (send, receive) = tokio::sync::oneshot::channel();
        let (actor, task) = crate::spawn_local_actor(None, Owner(Some(send)))
            .await
            .unwrap();
        let context = receive.await.unwrap();
        let jobs = CommandJobs::default();
        let (job, request) = jobs
            .start(
                &context,
                CommandSpec {
                    argv: vec!["fixture-command".into()],
                    directory: None,
                    environment: Vec::new(),
                    memory: 64 * 1024 * 1024,
                    input: CommandInput::ClosedInput,
                },
            )
            .await
            .unwrap();
        let backend = Arc::new(Backend::default());
        request.supply(Ok(backend.clone()));
        jobs.finished(&job).await.unwrap();
        Self {
            jobs,
            actor,
            task,
            backend,
            job,
        }
    }

    fn request(&self, budget: usize) -> BackgroundCommandRequest {
        BackgroundCommandRequest {
            job: self.job.clone(),
            binding: "retainedJob".into(),
            reason: CommandObservationStop::Deadline,
            named_tool: false,
            command_prefix: "earlier command output".into(),
            display_remaining: budget,
        }
    }

    async fn finish(self) {
        let terminal = self
            .actor
            .retire_by(
                self.actor.identity(),
                ActorTerminal {
                    kind: ActorExitKind::Cancelled,
                    summary: "background observation fixture complete".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(terminal.kind, ActorExitKind::Cancelled);
        self.task.await.unwrap();
    }
}

#[tokio::test]
async fn dropped_preparation_keeps_snapshot_pages_and_retained_job() {
    let f = Fixture::start().await;
    let actor = f.actor.identity();
    let pages = f.jobs.observation(actor, &f.job).await.unwrap();
    f.backend.reads.store(0, Ordering::Relaxed);
    let prepared = prepare(&f.jobs, actor, f.request(4096)).await;
    assert_eq!(f.backend.reads.load(Ordering::Relaxed), 2);
    assert_eq!(f.jobs.observation(actor, &f.job).await.unwrap(), pages);
    drop(prepared);
    assert_eq!(f.jobs.observation(actor, &f.job).await.unwrap(), pages);
    assert_eq!(f.jobs.owner(&f.job).unwrap(), actor);
    f.finish().await;
}

#[tokio::test]
async fn matching_apply_consumes_only_display_cursor_and_keeps_explicit_pages() {
    let f = Fixture::start().await;
    let actor = f.actor.identity();
    let prepared = prepare(&f.jobs, actor, f.request(4096)).await;
    let reads = f.backend.reads.load(Ordering::Relaxed);
    let output = prepared.apply(&f.jobs, actor);
    assert!(output.contains("retained stdout") && output.contains("retained stderr"));
    assert_eq!(f.backend.reads.load(Ordering::Relaxed), reads);
    assert!(f.jobs.observation(actor, &f.job).await.unwrap().is_empty());
    let other = ActorRef {
        incarnation: crate::Incarnation(actor.incarnation.0 + 1),
        ..actor
    };
    assert_eq!(f.jobs.observation(other, &f.job).await.unwrap().len(), 2);
    assert_eq!(
        f.jobs
            .read(
                actor,
                &f.job,
                CommandStream::Stdout,
                CommandPosition::OutputBeginning
            )
            .await
            .unwrap()
            .text,
        "retained stdout\n"
    );
    f.finish().await;
}

#[tokio::test]
async fn foreign_incarnation_apply_preserves_both_display_cursors() {
    let f = Fixture::start().await;
    let actor = f.actor.identity();
    let other = ActorRef {
        incarnation: crate::Incarnation(actor.incarnation.0 + 1),
        ..actor
    };
    let prepared = prepare(&f.jobs, actor, f.request(4096)).await;
    let output = prepared.apply(&f.jobs, other);
    assert!(output.contains("retained stdout"));
    assert!(output.contains("Output cursor unavailable: CommandUnauthorized"));
    assert_eq!(f.jobs.observation(actor, &f.job).await.unwrap().len(), 2);
    assert_eq!(f.jobs.observation(other, &f.job).await.unwrap().len(), 2);
    f.finish().await;
}

#[tokio::test]
async fn exhausted_display_budget_does_not_consume_unrendered_pages() {
    let f = Fixture::start().await;
    let actor = f.actor.identity();
    let prepared = prepare(&f.jobs, actor, f.request(0)).await;
    let output = prepared.apply(&f.jobs, actor);
    assert!(!output.contains("retained stdout") && !output.contains("retained stderr"));
    assert!(output.contains("earlier command output"));
    assert_eq!(f.jobs.observation(actor, &f.job).await.unwrap().len(), 2);
    f.finish().await;
}

#[tokio::test]
async fn unavailable_observation_preserves_job_and_can_be_read_after_backend_recovers() {
    let f = Fixture::start().await;
    let actor = f.actor.identity();
    f.backend.unavailable.store(true, Ordering::Relaxed);
    let output = prepare(&f.jobs, actor, f.request(4096))
        .await
        .apply(&f.jobs, actor);
    assert!(output.contains("Output unavailable:"));
    assert_eq!(f.jobs.owner(&f.job).unwrap(), actor);
    f.backend.unavailable.store(false, Ordering::Relaxed);
    assert_eq!(f.jobs.observation(actor, &f.job).await.unwrap().len(), 2);
    f.finish().await;
}
