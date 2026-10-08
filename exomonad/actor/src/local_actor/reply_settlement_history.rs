//! Kernel delivery and request observation are distinct linearization points.
//! The test behavior supplies a controlled continuation, not a native reply
//! authority. Native custody and resident wiring remain in terminal_transfer_tests.

use super::*;
use crate::request::{RequestRegistry, ResponseFailure, ResponseObservation, WatchObservation};
use proptest::prelude::*;
use proptest::test_runner::{Config, FileFailurePersistence, TestRunner};
use std::cell::RefCell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tidepool_runtime::session::{WorkbenchRequest, WorkbenchResponse, WorkbenchRunStatus};
use tokio::sync::{oneshot, Notify};

struct ReplyDriver {
    registry: Arc<RequestRegistry>,
    request: Arc<parking_lot::Mutex<Option<crate::RequestId>>>,
    continuation: Option<oneshot::Receiver<bool>>,
    entered: Arc<Notify>,
    applied: Arc<Notify>,
    resumes: Arc<AtomicUsize>,
    accepted: bool,
}

fn response(status: WorkbenchRunStatus) -> WorkbenchResponse {
    WorkbenchResponse {
        status,
        summary: None,
        items: Vec::new(),
        next_index: 0,
        total: 0,
        publication: None,
    }
}

impl KernelBehavior for ReplyDriver {
    fn start<'a>(
        &'a mut self,
        _: &'a KernelContext,
    ) -> BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>> {
        Box::pin(async { Ok(KernelStep::Continue(())) })
    }
    fn cast<'a>(
        &'a mut self,
        _: &'a KernelContext,
        _: ActorRef,
        _: MailboxValue,
    ) -> BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>> {
        Box::pin(async { Ok(KernelStep::Continue(())) })
    }
    fn call<'a>(
        &'a mut self,
        _: &'a KernelContext,
        _: ActorRef,
        _: crate::CallAncestry,
        value: MailboxValue,
    ) -> BoxFuture<'a, Result<KernelStep<MailboxValue>, KernelBehaviorError>> {
        Box::pin(async move { Ok(KernelStep::Continue(value)) })
    }
    fn tool<'a>(
        &'a mut self,
        context: &'a KernelContext,
        _: exomonad_tool::ToolInvocation,
        _: Option<Arc<dyn crate::HostedCheckpointCapture>>,
    ) -> BoxFuture<'a, Result<KernelStep<serde_json::Value>, KernelInvocationFailure>> {
        Box::pin(async move {
            Err(KernelInvocationFailure::Rejected {
                receipts: Vec::new(),
                actor: context.identity(),
                detail: "no tool surface".into(),
                diagnostic: None,
            })
        })
    }
    fn workbench<'a>(
        &'a mut self,
        context: &'a KernelContext,
        _: crate::ActorWorkbenchInvocation,
        _: Option<Arc<crate::WorkbenchExecutionControl>>,
    ) -> BoxFuture<'a, Result<KernelStep<WorkbenchResponse>, KernelInvocationFailure>> {
        Box::pin(async move {
            let request = self.request.lock().expect("fixture request admitted");
            let result = self.registry.begin_reply(context.identity(), request);
            if self.accepted {
                assert!(
                    matches!(
                        result,
                        Err(crate::ReplyError::Stale | crate::ReplyError::AlreadySettled)
                    ),
                    "duplicate reply: {result:?}"
                );
                return Ok(KernelStep::Continue(response(
                    WorkbenchRunStatus::Committed,
                )));
            }
            result.expect("presented request accepts one reply");
            self.accepted = true;
            Ok(KernelStep::ContinueLater(response(
                WorkbenchRunStatus::Replied,
            )))
        })
    }
    fn dispatch_resume(
        &mut self,
        context: &KernelContext,
        kind: crate::kernel::KernelResume,
    ) -> Result<OwnedActorTask<Self, ()>, KernelBehaviorError> {
        assert_eq!(kind, crate::kernel::KernelResume::ContinueProgram);
        self.resumes.fetch_add(1, Ordering::SeqCst);
        let completion = self
            .continuation
            .take()
            .expect("one continuation per accepted reply");
        let entered = self.entered.clone();
        let context = context.clone();
        Ok(OwnedActorTask::new(Box::pin(async move {
            entered.notify_one();
            let successful = tokio::select! {
                completed = completion => Some(completed.expect("controlled continuation released")),
                _ = context.wait_requested_shutdown() => None,
            };
            OwnedActorCompletion::new(move |driver: &mut Self| {
                let request = driver.request.lock().expect("request retained");
                match successful {
                    Some(true) => {
                        driver.registry.finish_reply(request, None);
                    }
                    Some(false) => {
                        driver
                            .registry
                            .fail_reply_settlement(request, "controlled continuation failure");
                    }
                    // The kernel defers shutdown while this task is parked.
                    // Release the task; the real shutdown hook owns closure.
                    None => {}
                }
                driver.applied.notify_one();
                Ok(KernelStep::Continue(()))
            })
        })))
    }
    fn external_application_failed<'a>(
        &'a mut self,
        _: &'a KernelContext,
        _: ExternalApplicationFailure,
    ) -> BoxFuture<'a, ExternalFailureDisposition> {
        Box::pin(async { ExternalFailureDisposition::Applied })
    }
    fn shutdown<'a>(
        &'a mut self,
        context: &'a KernelContext,
        terminal: &'a ActorTerminal,
    ) -> BoxFuture<'a, Result<(), KernelBehaviorError>> {
        Box::pin(async move {
            self.registry.actor_stopped(context.identity(), terminal);
            Ok(())
        })
    }
    fn shutdown_components<'a>(
        &'a mut self,
        context: &'a KernelContext,
        terminal: &'a ActorTerminal,
        _: tokio::time::Instant,
    ) -> BoxFuture<
        'a,
        (
            crate::CleanupComponentOutcome,
            crate::CleanupComponentOutcome,
        ),
    > {
        Box::pin(async move {
            self.shutdown(context, terminal).await.unwrap();
            (
                crate::CleanupComponentOutcome::Confirmed,
                crate::CleanupComponentOutcome::Confirmed,
            )
        })
    }
    fn stopped<'a>(&'a mut self, _: &'a KernelContext, _: &'a ActorTerminal) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
    fn child_exited(&mut self, _: ChildExitNotice) {}
}

#[derive(Clone, Copy, Debug)]
enum ReadOperation {
    Observe,
    Subscribe,
    DuplicateReply,
    StaleCompletion,
    StaleSettlement,
}
#[derive(Clone, Copy, Debug)]
enum Completion {
    Success,
    Failure,
    Shutdown,
}
#[derive(Clone, Debug)]
struct History {
    before: Vec<ReadOperation>,
    completion: Completion,
    after: Vec<ReadOperation>,
}

fn histories() -> impl Strategy<Value = History> {
    let reads = || {
        prop_oneof![
            Just(ReadOperation::Observe),
            Just(ReadOperation::Subscribe),
            Just(ReadOperation::DuplicateReply),
            Just(ReadOperation::StaleCompletion),
            Just(ReadOperation::StaleSettlement)
        ]
    };
    (
        proptest::collection::vec(reads(), 0..8),
        prop_oneof![
            Just(Completion::Success),
            Just(Completion::Failure),
            Just(Completion::Shutdown)
        ],
        proptest::collection::vec(reads(), 0..8),
    )
        .prop_map(|(before, completion, after)| History {
            before,
            completion,
            after,
        })
}

#[derive(Default, Debug)]
struct Coverage {
    replay_callbacks: usize,
    observations: usize,
    subscriptions: usize,
    duplicates: usize,
    stale: usize,
    stale_settlements: usize,
    successes: usize,
    failures: usize,
    shutdowns: usize,
}

fn send_workbench(
    actor: &LocalActorRef,
    control: Option<Arc<crate::WorkbenchExecutionControl>>,
) -> oneshot::Receiver<crate::KernelWorkbenchReply> {
    let (send, receive) = oneshot::channel();
    actor
        .address()
        .send_message(KernelMessage::Workbench {
            invocation: crate::ActorWorkbenchInvocation::unbound(
                WorkbenchRequest::from_cell_input("reply boundary"),
            ),
            control,
            reply: send.into(),
        })
        .unwrap();
    receive
}

fn check_observation(
    registry: &RequestRegistry,
    owner: ActorRef,
    request: crate::RequestId,
    completed: Option<Completion>,
) {
    let observed = registry.observe_response(owner, request).unwrap();
    match completed {
        None => assert!(
            matches!(observed, ResponseObservation::Pending(_)),
            "{observed:?}"
        ),
        Some(Completion::Success) => assert_eq!(observed, ResponseObservation::Ready),
        Some(Completion::Failure) => assert_eq!(
            observed,
            ResponseObservation::Unavailable(ResponseFailure::SettlementFailed(
                "controlled continuation failure".into()
            ))
        ),
        Some(Completion::Shutdown) => assert_eq!(
            observed,
            ResponseObservation::Unavailable(ResponseFailure::TargetCancelled(
                "controlled shutdown".into()
            ))
        ),
    }
}

async fn run_history(history: &History, coverage: &mut Coverage) {
    coverage.replay_callbacks += 1;
    let registry = Arc::new(RequestRegistry::default());
    let request_slot = Arc::new(parking_lot::Mutex::new(None));
    let (release, continuation) = oneshot::channel();
    let entered = Arc::new(Notify::new());
    let applied = Arc::new(Notify::new());
    let resumes = Arc::new(AtomicUsize::new(0));
    let (actor, task) = spawn_local_actor(
        None,
        ReplyDriver {
            registry: registry.clone(),
            request: request_slot.clone(),
            continuation: Some(continuation),
            entered: entered.clone(),
            applied: applied.clone(),
            resumes: resumes.clone(),
            accepted: false,
        },
    )
    .await
    .unwrap();
    let owner = ActorRef::first(crate::ActorId(u64::MAX - 1));
    assert_ne!(owner, actor.identity());
    let request = registry.reserve(owner, actor.identity());
    *request_slot.lock() = Some(request);
    registry
        .mark_queued(owner, actor.identity(), request)
        .unwrap();
    registry.present(actor.identity(), request).unwrap();
    check_observation(&registry, owner, request, None);
    let (watch, _) = registry.register_watch(owner, vec![request]).unwrap();
    let mut subscriptions = vec![registry.subscribe_watch(owner, watch).unwrap()];
    let control = crate::WorkbenchExecutionControl::untracked();
    let reply = send_workbench(&actor, Some(control.clone()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reply.status, WorkbenchRunStatus::Replied);
    assert!(matches!(control.terminal_reply(), Some(Ok(_))));
    entered.notified().await;
    assert_eq!(resumes.load(Ordering::SeqCst), 1);
    check_observation(&registry, owner, request, None);

    // The operation interpreter lives outside the behavior: expected readiness
    // comes from acknowledged workflow facts, never its registry state table.
    for operation in &history.before {
        read_operation(
            *operation,
            &actor,
            &registry,
            owner,
            request,
            watch,
            None,
            &mut subscriptions,
            coverage,
        )
        .await;
    }
    match history.completion {
        Completion::Success | Completion::Failure => {
            let successful = matches!(history.completion, Completion::Success);
            release.send(successful).unwrap();
            applied.notified().await;
            if successful {
                coverage.successes += 1;
            } else {
                coverage.failures += 1;
            }
        }
        Completion::Shutdown => {
            let shutdown = actor
                .shutdown_with_cleanup(ActorTerminal {
                    kind: ActorExitKind::Cancelled,
                    summary: "controlled shutdown".into(),
                    diagnostic: None,
                })
                .await
                .unwrap();
            assert!(shutdown.cleanup.is_confirmed());
            drop(release);
            coverage.shutdowns += 1;
        }
    }
    check_observation(&registry, owner, request, Some(history.completion));
    for operation in &history.after {
        read_operation(
            *operation,
            &actor,
            &registry,
            owner,
            request,
            watch,
            Some(history.completion),
            &mut subscriptions,
            coverage,
        )
        .await;
    }
    for subscription in subscriptions {
        let observed = subscription.wait().await.unwrap();
        match history.completion {
            Completion::Success => assert_eq!(observed, WatchObservation::Ready(Vec::new())),
            Completion::Failure => assert_eq!(
                observed,
                WatchObservation::Ready(vec![(
                    request,
                    ResponseFailure::SettlementFailed("controlled continuation failure".into())
                )])
            ),
            Completion::Shutdown => assert_eq!(
                observed,
                WatchObservation::Ready(vec![(
                    request,
                    ResponseFailure::TargetCancelled("controlled shutdown".into())
                )])
            ),
        }
    }
    // Opposite and repeated completions cannot overwrite the first outcome.
    registry.finish_reply(request, None);
    registry.fail_reply_settlement(request, "late failure");
    check_observation(&registry, owner, request, Some(history.completion));
    assert_eq!(resumes.load(Ordering::SeqCst), 1);
    if !matches!(history.completion, Completion::Shutdown) {
        let shutdown = actor
            .shutdown_with_cleanup(ActorTerminal {
                kind: ActorExitKind::Completed,
                summary: "history complete".into(),
                diagnostic: None,
            })
            .await
            .unwrap();
        assert!(shutdown.cleanup.is_confirmed());
        check_observation(&registry, owner, request, Some(history.completion));
    }
    task.await.unwrap();
}

#[allow(clippy::too_many_arguments)]
async fn read_operation(
    operation: ReadOperation,
    actor: &LocalActorRef,
    registry: &Arc<RequestRegistry>,
    owner: ActorRef,
    request: crate::RequestId,
    watch: crate::WatchId,
    completed: Option<Completion>,
    subscriptions: &mut Vec<crate::request::WatchWaitSubscription>,
    coverage: &mut Coverage,
) {
    match operation {
        ReadOperation::Observe => coverage.observations += 1,
        ReadOperation::Subscribe => {
            subscriptions.push(registry.subscribe_watch(owner, watch).unwrap());
            coverage.subscriptions += 1;
        }
        ReadOperation::DuplicateReply => {
            match completed {
                None => assert_eq!(
                    registry.begin_reply(actor.identity(), request),
                    Err(crate::ReplyError::Stale)
                ),
                Some(Completion::Shutdown) => assert_eq!(
                    registry.begin_reply(actor.identity(), request),
                    Err(crate::ReplyError::AlreadySettled)
                ),
                Some(_) => assert_eq!(
                    send_workbench(actor, None).await.unwrap().unwrap().status,
                    WorkbenchRunStatus::Committed
                ),
            }
            coverage.duplicates += 1;
        }
        ReadOperation::StaleSettlement => {
            let missing = crate::RequestId(u64::MAX);
            assert_ne!(request, missing);
            assert!(registry.finish_reply(missing, None).is_empty());
            assert!(registry
                .fail_reply_settlement(missing, "stale failure")
                .is_empty());
            assert_eq!(
                registry.observe_response(owner, missing),
                Err(crate::ReplyError::Stale)
            );
            coverage.stale_settlements += 1;
        }
        ReadOperation::StaleCompletion => {
            if !matches!(completed, Some(Completion::Shutdown)) {
                actor
                    .address()
                    .send_message(KernelMessage::ActorStepCompleted {
                        step: crate::WorkbenchStepKey::new(actor.identity(), u64::MAX, None),
                        outcome: Box::new(()),
                    })
                    .unwrap();
                // While the exclusive continuation is held, its later real
                // completion is the acknowledgment: the stale message was
                // enqueued before release. After settlement, use an RPC fence.
                if completed.is_some() {
                    assert_eq!(
                        send_workbench(actor, None).await.unwrap().unwrap().status,
                        WorkbenchRunStatus::Committed
                    );
                }
            }
            if !matches!(completed, Some(Completion::Shutdown)) {
                coverage.stale += 1;
            }
        }
    }
    check_observation(registry, owner, request, completed);
}

#[test]
fn fixed_reply_delivery_histories_preserve_pending_until_continuation_settles() {
    let mut coverage = Coverage::default();
    for completion in [
        Completion::Success,
        Completion::Failure,
        Completion::Shutdown,
    ] {
        let operations = vec![
            ReadOperation::Observe,
            ReadOperation::Subscribe,
            ReadOperation::DuplicateReply,
            ReadOperation::StaleCompletion,
            ReadOperation::StaleSettlement,
        ];
        replay(
            &History {
                before: operations.clone(),
                completion,
                after: operations,
            },
            &mut coverage,
        )
        .unwrap();
    }
    assert_eq!(
        (coverage.successes, coverage.failures, coverage.shutdowns),
        (1, 1, 1)
    );
    eprintln!("reply deterministic coverage: {coverage:?}");
}

#[test]
fn generated_reply_delivery_histories_match_acknowledged_settlement() {
    let mut config = Config::default();
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = 48;
    }
    if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
        config.failure_persistence = Some(Box::new(FileFailurePersistence::Direct(path)));
    }
    let mut config = proptest::test_runner::contextualize_config(config);
    config.source_file = Some(file!());
    config.test_name = Some(concat!(
        module_path!(),
        "::generated_reply_delivery_histories_match_acknowledged_settlement"
    ));
    eprintln!("reply configured fresh cases: {}", config.cases);
    let coverage = RefCell::new(Coverage::default());
    let result = TestRunner::new(config).run(&histories(), |history| {
        replay(&history, &mut coverage.borrow_mut())
    });
    eprintln!("reply generated coverage: {:?}", coverage.borrow());
    result.unwrap();
}

// Each replay owns its runtime, including a failed/shrunk replay. A failure
// cannot leave parked actors in the next candidate's scheduler.
fn replay(
    history: &History,
    coverage: &mut Coverage,
) -> Result<(), proptest::test_runner::TestCaseError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                run_history(history, coverage),
            )
            .await
            .expect("acknowledged reply history must complete");
        });
    }))
    .map_err(|panic| {
        let detail = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied())
            .unwrap_or("non-string history failure");
        proptest::test_runner::TestCaseError::fail(format!("{history:?}: {detail}"))
    })
}
