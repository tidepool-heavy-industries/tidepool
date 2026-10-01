use super::*;
use crate::command_jobs::{CommandBackend, CommandControl};
use crate::request::{ResponseObservation, WatchRequirement, WorkbenchReservationAttempt};
use futures_util::future::BoxFuture;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tidepool_bridge_effects::{
    CommandCleanup, CommandError, CommandInput, CommandOutcome, CommandOutput, CommandPage,
    CommandPosition, CommandResult, CommandSpec, CommandStatus, CommandStream,
};

fn reservation() -> RequestReservationOwner {
    RequestReservationOwner::Workbench {
        execution: WorkbenchExecutionId::from_digest([7; 16]),
        attempt: WorkbenchReservationAttempt::fresh(),
    }
}

#[test]
fn invocation_membership_fences_actor_incarnation_and_reservation_attempt() {
    let owner = ActorRef::first(crate::ActorId(1));
    let reservation = reservation();
    let work = InvocationWork::new(owner, reservation.clone());
    assert!(work.matches(owner, &reservation));
    assert!(!work.matches(ActorRef::first(crate::ActorId(2)), &reservation));
    assert!(!work.matches(
        ActorRef {
            incarnation: crate::Incarnation(owner.incarnation.0 + 1),
            ..owner
        },
        &reservation,
    ));
    assert!(!work.matches(owner, &self::reservation()));
    assert!(!work.matches(owner, &RequestReservationOwner::Route(crate::WatchId(7))));

    let route = RequestReservationOwner::Route(crate::WatchId(7));
    let route_work = InvocationWork::new(owner, route.clone());
    assert!(route_work.matches(owner, &route));
    assert!(!route_work.matches(owner, &RequestReservationOwner::Route(crate::WatchId(8))));
    assert!(!route_work.matches(owner, &reservation));
}

struct Owner(Option<tokio::sync::oneshot::Sender<KernelContext>>);

impl crate::KernelBehavior for Owner {
    fn start<'a>(
        &'a mut self,
        context: &'a KernelContext,
    ) -> BoxFuture<'a, Result<crate::KernelStep<()>, crate::KernelBehaviorError>> {
        assert!(self.0.take().unwrap().send(context.clone()).is_ok());
        Box::pin(async { Ok(crate::KernelStep::Continue(())) })
    }

    fn cast<'a>(
        &'a mut self,
        _: &'a KernelContext,
        _: ActorRef,
        _: crate::MailboxValue,
    ) -> BoxFuture<'a, Result<crate::KernelStep<()>, crate::KernelBehaviorError>> {
        Box::pin(async { panic!("fixture has no mailbox casts") })
    }

    fn call<'a>(
        &'a mut self,
        _: &'a KernelContext,
        _: ActorRef,
        _: crate::CallAncestry,
        _: crate::MailboxValue,
    ) -> BoxFuture<'a, Result<crate::KernelStep<crate::MailboxValue>, crate::KernelBehaviorError>>
    {
        Box::pin(async { panic!("fixture has no mailbox calls") })
    }

    fn tool<'a>(
        &'a mut self,
        _: &'a KernelContext,
        _: exomonad_tool::ToolInvocation,
        _: Option<Arc<dyn crate::HostedCheckpointCapture>>,
    ) -> BoxFuture<'a, Result<crate::KernelStep<serde_json::Value>, crate::KernelInvocationFailure>>
    {
        Box::pin(async { panic!("fixture has no tools") })
    }

    fn workbench<'a>(
        &'a mut self,
        _: &'a KernelContext,
        _: crate::ActorWorkbenchInvocation,
        _: Option<Arc<crate::WorkbenchExecutionControl>>,
    ) -> BoxFuture<'a, Result<crate::KernelStep<WorkbenchResponse>, crate::KernelInvocationFailure>>
    {
        Box::pin(async { panic!("fixture has no workbench") })
    }

    fn external_application_failed<'a>(
        &'a mut self,
        _: &'a KernelContext,
        _: crate::ExternalApplicationFailure,
    ) -> BoxFuture<'a, crate::ExternalFailureDisposition> {
        Box::pin(async { panic!("fixture has no external application") })
    }

    fn shutdown<'a>(
        &'a mut self,
        _: &'a KernelContext,
        _: &'a ActorTerminal,
    ) -> BoxFuture<'a, Result<(), crate::KernelBehaviorError>> {
        Box::pin(async { Ok(()) })
    }

    fn stopped<'a>(&'a mut self, _: &'a KernelContext, _: &'a ActorTerminal) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn child_exited(&mut self, _: crate::ChildExitNotice) {}
}

struct Fixture {
    actor: LocalActorRef,
    task: ractor::concurrency::JoinHandle<()>,
    kernel: KernelContext,
    environment: ResidentEnvironment<frunk::HNil, tidepool_mcp::CapturedOutput>,
}

impl Fixture {
    async fn start() -> Self {
        let (send, receive) = tokio::sync::oneshot::channel();
        let (actor, task) = crate::spawn_local_actor(None, Owner(Some(send)))
            .await
            .unwrap();
        let kernel = receive.await.unwrap();
        let (deployments, _receiver) = mpsc::channel(1);
        let environment = ResidentEnvironment {
            runner: ResidentActorRunner::new(
                Arc::new(ActorMachineRegistry::new()),
                ActorWorkbenchSource::new("", Vec::new()),
            ),
            deployments,
            retired: Default::default(),
            requests: Default::default(),
            commands: Default::default(),
            fork_groups: crate::ForkGroupRegistry::new(crate::ActorLineageRegistry::default()),
            actors: Default::default(),
            fork_workspaces: None,
            root_admission_closed: Default::default(),
            launch_resolver: None,
            source_layers: None,
            jev: Arc::new(crate::jev::UnconfiguredJev),
            release_tracked: Default::default(),
            conversation_reader: None,
            usage_pointers: Default::default(),
            recovery: None,
        };
        Self {
            actor,
            task,
            kernel,
            environment,
        }
    }

    async fn cleanup(&self, work: &InvocationWork) {
        let cleanup = tokio::time::timeout(
            Duration::from_secs(1),
            work.cleanup(&self.environment, &self.kernel),
        )
        .await
        .expect("responsive owners must finish invocation cleanup");
        assert_eq!(cleanup.uncertainty(), None);
        assert_eq!(work.cleanup_observation().unwrap().uncertainty(), None);
    }

    async fn pending_command(&self) -> (String, Arc<PendingBackend>) {
        let (job, request) = self
            .environment
            .commands
            .start(
                &self.kernel,
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
        let backend = Arc::new(PendingBackend::default());
        request.supply(Ok(backend.clone()));
        tokio::time::timeout(
            Duration::from_secs(1),
            self.environment.commands.supplied(&job),
        )
        .await
        .unwrap()
        .unwrap();
        (job, backend)
    }

    async fn finish(self) {
        self.actor
            .retire_by(
                self.actor.identity(),
                ActorTerminal {
                    kind: ActorExitKind::Cancelled,
                    summary: "invocation fixture complete".into(),
                },
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), self.task)
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn closed_invocation_refuses_every_work_registration() {
    let fixture = Fixture::start().await;
    let work = InvocationWork::new(fixture.actor.identity(), reservation());
    work.close();
    assert!(matches!(
        work.register_command("late-command".into()),
        Err(CommandError::CommandUnavailable(_))
    ));
    assert_eq!(
        work.register_transient_watch(crate::WatchId(1)),
        Err(crate::ReplyError::CancellationRequested)
    );
    assert!(work.register_worker(fixture.actor.clone()).is_err());
    assert!(work.register_group(crate::ForkGroupId(1)).is_err());
    fixture.cleanup(&work).await;
    assert!(
        fixture.actor.terminal().get().is_none(),
        "refused worker registration must not retire it"
    );
    fixture.finish().await;
}

#[tokio::test]
async fn invocation_cleanup_releases_transient_watch_without_cancelling_target() {
    let fixture = Fixture::start().await;
    let owner = fixture.actor.identity();
    let target = ActorRef::first(crate::ActorId(owner.id.0 + 100));
    let requests = &fixture.environment.requests;
    let request = requests.reserve(owner, target);
    requests.mark_queued(owner, target, request).unwrap();
    requests.present(target, request).unwrap();
    let watch = requests
        .register_transient_watch(
            owner,
            vec![vec![(
                request,
                WatchRequirement::Response {
                    allow_failure: false,
                },
            )]],
        )
        .unwrap();
    let subscription = requests.subscribe_watch(owner, watch).unwrap();
    let work = InvocationWork::new(owner, reservation());
    work.register_transient_watch(watch).unwrap();

    fixture.cleanup(&work).await;

    assert!(!requests.retains_watch(owner, watch));
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), subscription.wait())
            .await
            .unwrap(),
        Err(crate::ReplyError::Stale)
    );
    assert!(matches!(
        requests.observe_response(owner, request),
        Ok(ResponseObservation::Pending(_))
    ));
    requests
        .begin_reply(target, request)
        .expect("target remains able to reply");
    requests.finish_reply(request, None);
    assert_eq!(
        requests.observe_response(owner, request),
        Ok(ResponseObservation::Ready)
    );
    fixture.cleanup(&work).await;
    fixture.finish().await;
}

struct PendingBackend {
    cancellations: AtomicUsize,
    finish: tokio::sync::Semaphore,
    cancel_entered: tokio::sync::Semaphore,
    cancel_gate: Mutex<Option<Arc<tokio::sync::Semaphore>>>,
}

impl Default for PendingBackend {
    fn default() -> Self {
        Self {
            cancellations: AtomicUsize::new(0),
            finish: tokio::sync::Semaphore::new(0),
            cancel_entered: tokio::sync::Semaphore::new(0),
            cancel_gate: Mutex::new(None),
        }
    }
}

impl CommandBackend for PendingBackend {
    fn execute<'a>(
        &'a self,
        _: &'a str,
        _: CommandSpec,
        _: tokio::sync::watch::Sender<CommandStatus>,
    ) -> BoxFuture<'a, CommandResult> {
        Box::pin(async move {
            self.finish.acquire().await.unwrap().forget();
            CommandResult {
                outcome: CommandOutcome::CommandExited(0),
                cleanup: CommandCleanup::CommandClean,
            }
        })
    }

    fn control<'a>(
        &'a self,
        _: &'a str,
        operation: CommandControl,
    ) -> BoxFuture<'a, Result<(), CommandError>> {
        Box::pin(async move {
            assert!(matches!(operation, CommandControl::Cancel));
            self.cancellations.fetch_add(1, Ordering::Relaxed);
            self.cancel_entered.add_permits(1);
            let gate = self.cancel_gate.lock().clone();
            if let Some(gate) = gate {
                gate.acquire().await.unwrap().forget();
            }
            self.finish.add_permits(1);
            Ok(())
        })
    }

    fn output<'a>(
        &'a self,
        _: &'a str,
        _: usize,
    ) -> BoxFuture<'a, Result<CommandOutput, CommandError>> {
        Box::pin(async { panic!("fixture has no output") })
    }

    fn read<'a>(
        &'a self,
        _: &'a str,
        _: CommandStream,
        _: CommandPosition,
    ) -> BoxFuture<'a, Result<CommandPage, CommandError>> {
        Box::pin(async { panic!("fixture has no output pages") })
    }

    fn cleanup<'a>(&'a self, _: &'a str) -> BoxFuture<'a, CommandCleanup> {
        Box::pin(async { CommandCleanup::CommandClean })
    }
}

#[tokio::test]
async fn owned_command_detach_survives_cleanup_and_borrowed_detach_is_unauthorized() {
    let fixture = Fixture::start().await;
    let owner = fixture.actor.identity();
    let jobs = &fixture.environment.commands;
    let (job, backend) = fixture.pending_command().await;
    let work = InvocationWork::new(owner, reservation());
    work.register_command(job.clone()).unwrap();
    let borrower = ActorRef::first(crate::ActorId(owner.id.0 + 100));
    assert_eq!(
        jobs.control(borrower, &job, CommandControl::Cancel).await,
        Err(CommandError::CommandUnauthorized)
    );
    assert_eq!(
        work.detach_command(jobs, borrower, &job),
        Err(CommandError::CommandUnauthorized)
    );
    assert_eq!(
        work.detach_command(
            jobs,
            ActorRef {
                incarnation: crate::Incarnation(owner.incarnation.0 + 1),
                ..owner
            },
            &job
        ),
        Err(CommandError::CommandUnauthorized)
    );
    let borrowed_work = InvocationWork::new(borrower, reservation());
    assert_eq!(
        borrowed_work.detach_command(jobs, borrower, &job),
        Err(CommandError::CommandUnauthorized)
    );
    work.detach_command(jobs, owner, &job).unwrap();

    fixture.cleanup(&work).await;

    assert_eq!(backend.cancellations.load(Ordering::Relaxed), 0);
    assert_eq!(jobs.owner(&job), Ok(owner));
    assert!(!matches!(
        jobs.status(owner, &job).await.unwrap(),
        CommandStatus::CommandFinished(_)
    ));
    backend.finish.add_permits(1);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), jobs.finished(&job))
            .await
            .unwrap()
            .unwrap()
            .cleanup,
        CommandCleanup::CommandClean
    );
    fixture.finish().await;
}

#[tokio::test]
async fn sibling_invocation_cannot_detach_same_actor_command() {
    let fixture = Fixture::start().await;
    let owner = fixture.actor.identity();
    let jobs = &fixture.environment.commands;
    let (job, backend) = fixture.pending_command().await;
    let first = InvocationWork::new(owner, reservation());
    let sibling = InvocationWork::new(owner, reservation());
    sibling.register_command(job.clone()).unwrap();

    assert_eq!(
        first.detach_command(jobs, owner, &job),
        Err(CommandError::CommandUnauthorized)
    );
    sibling.detach_command(jobs, owner, &job).unwrap();
    sibling
        .detach_command(jobs, owner, &job)
        .expect("own detach is idempotent");
    fixture.cleanup(&first).await;
    fixture.cleanup(&sibling).await;
    assert_eq!(backend.cancellations.load(Ordering::Relaxed), 0);
    backend.finish.add_permits(1);
    tokio::time::timeout(Duration::from_secs(1), jobs.finished(&job))
        .await
        .unwrap()
        .unwrap();
    fixture.finish().await;
}

#[tokio::test]
async fn concurrent_invocation_cleanup_cancels_owned_command_once() {
    let fixture = Fixture::start().await;
    let owner = fixture.actor.identity();
    let (job, backend) = fixture.pending_command().await;
    let work = InvocationWork::new(owner, reservation());
    work.register_command(job).unwrap();
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    *backend.cancel_gate.lock() = Some(gate.clone());

    tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(fixture.cleanup(&work), fixture.cleanup(&work), async {
            backend.cancel_entered.acquire().await.unwrap().forget();
            tokio::task::yield_now().await;
            gate.add_permits(1);
        },);
    })
    .await
    .expect("concurrent cleanup must share the completed observation");

    assert_eq!(backend.cancellations.load(Ordering::Relaxed), 1);
    *backend.cancel_gate.lock() = None;
    fixture.finish().await;
}

#[tokio::test]
async fn invocation_cleanup_preserves_completed_worker_terminal() {
    let fixture = Fixture::start().await;
    let (send, receive) = tokio::sync::oneshot::channel();
    let (worker, task) = crate::spawn_local_actor(None, Owner(Some(send)))
        .await
        .unwrap();
    let _worker_context = receive.await.unwrap();
    let work = InvocationWork::new(fixture.actor.identity(), reservation());
    work.register_worker(worker.clone()).unwrap();
    let completed = ActorTerminal {
        kind: ActorExitKind::Completed,
        summary: "worker result retained".into(),
    };
    let shutdown = worker
        .shutdown_with_cleanup(completed.clone())
        .await
        .unwrap();
    assert_eq!(shutdown.terminal, completed);
    assert!(shutdown.cleanup.is_confirmed());
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap();

    fixture.cleanup(&work).await;

    assert_eq!(worker.terminal().get(), Some(completed));
    assert!(worker.terminal().cleanup().unwrap().is_confirmed());
    fixture.finish().await;
}

#[test]
fn unconfirmed_invocation_cleanup_preserves_committed_reply_and_receipts() {
    let cleanup = InvocationCleanup {
        commands: vec![InvocationCommandCleanup {
            job: "retained-command".into(),
            result: Some(CommandResult {
                outcome: CommandOutcome::CommandExited(0),
                cleanup: CommandCleanup::CommandRetained,
            }),
            failure: None,
        }],
        ..Default::default()
    };
    let receipt = WorkbenchItemReceipt {
        index: 0,
        kind: None,
        span: None,
        source_items: Vec::new(),
        status: WorkbenchItemStatus::Committed,
        output: "committed reply".into(),
        diagnostics: Vec::new(),
        failure_layer: None,
        warnings: Vec::new(),
        installed_bindings: vec!["answer".into()],
        operations: Vec::new(),
        terminal_transfer: None,
    };
    let response = WorkbenchResponse {
        status: WorkbenchRunStatus::Committed,
        summary: Some("reply accepted".into()),
        items: vec![receipt.clone()],
        next_index: 1,
        total: 1,
    };
    let terminal = ActorTerminal {
        kind: ActorExitKind::Completed,
        summary: "reply complete".into(),
    };
    for (expected_variant, step) in [
        (0, KernelStep::Continue(response.clone())),
        (1, KernelStep::ContinueLater(response.clone())),
        (
            2,
            KernelStep::Stop {
                output: response.clone(),
                terminal: terminal.clone(),
            },
        ),
    ] {
        let result = retain_invocation_cleanup_summary(Ok(step), cleanup.uncertainty());
        let Ok(step) = result else {
            panic!("committed reply remains successful")
        };
        let (variant, response) = match step {
            KernelStep::Continue(response) => (0, response),
            KernelStep::ContinueLater(response) => (1, response),
            KernelStep::Stop {
                output,
                terminal: actual_terminal,
            } => {
                assert_eq!(actual_terminal, terminal);
                (2, output)
            }
        };
        assert_eq!(variant, expected_variant);
        assert_eq!(response.status, WorkbenchRunStatus::Committed);
        assert_eq!(response.items, vec![receipt.clone()]);
        assert_eq!((response.next_index, response.total), (1, 1));
        let summary = response.summary.unwrap();
        assert!(summary.starts_with("reply accepted\n"));
        assert!(summary.contains("retained-command"));
    }
    assert!(
        cleanup.uncertainty().is_some(),
        "presenting uncertainty cannot settle the typed cleanup observation"
    );
    assert!(matches!(
        &cleanup.commands[0].result,
        Some(CommandResult {
            cleanup: CommandCleanup::CommandRetained,
            ..
        })
    ));
}

#[tokio::test]
async fn invocation_cleanup_cancels_owned_command_once_and_closes_detach() {
    let fixture = Fixture::start().await;
    let owner = fixture.actor.identity();
    let jobs = &fixture.environment.commands;
    let (job, backend) = fixture.pending_command().await;
    let work = InvocationWork::new(owner, reservation());
    work.register_command(job.clone()).unwrap();
    work.register_command(job.clone()).unwrap();
    work.close();
    assert!(matches!(
        work.detach_command(jobs, owner, &job),
        Err(CommandError::CommandUnavailable(_))
    ));

    fixture.cleanup(&work).await;

    assert_eq!(backend.cancellations.load(Ordering::Relaxed), 1);
    assert!(matches!(
        jobs.status(owner, &job).await.unwrap(),
        CommandStatus::CommandFinished(CommandResult {
            cleanup: CommandCleanup::CommandClean,
            ..
        })
    ));
    fixture.cleanup(&work).await;
    assert_eq!(backend.cancellations.load(Ordering::Relaxed), 1);
    fixture.finish().await;
}
