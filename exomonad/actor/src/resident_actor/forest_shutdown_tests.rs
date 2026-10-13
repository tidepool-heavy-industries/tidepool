use super::*;
use futures_util::future::BoxFuture;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tidepool_runtime::session::{ModuleEnv, SessionLib};

use super::test_forest::{Forest, TestForest as Fixture};

type Behavior = ResidentKernelBehavior<frunk::HNil, tidepool_mcp::CapturedOutput>;

impl Fixture {
    fn behavior(&self, descriptor: ActorDescriptor) -> Behavior {
        Behavior::with_boot(
            descriptor,
            self.forest.environment.clone(),
            ResidentBoot::Workbench,
            Vec::new(),
        )
    }

    async fn child_placement(&self, parent: &LocalActorRef) -> crate::ActorPlacement {
        let context = self
            .forest
            .directory
            .session_context(parent.identity())
            .await
            .unwrap()
            .expect("admitted parent retains its session context");
        let machines = self.forest.environment.runner.machines_for_test();
        let mut checkout = machines.checkout_run(context.placement.session).unwrap();
        let lexical_scope = checkout
            .machine()
            .mint_scope(context.placement.lexical_scope)
            .unwrap();
        let holes = checkout
            .machine()
            .parked_holes()
            .into_iter()
            .map(str::to_owned)
            .collect();
        checkout.restore_suspended(holes);
        crate::ActorPlacement {
            session: context.placement.session,
            resource_scope: RealmId::fresh(),
            lexical_scope,
        }
    }

    fn scope_is_live(&self, placement: crate::ActorPlacement) -> bool {
        let machines = self.forest.environment.runner.machines_for_test();
        let mut checkout = machines.checkout_run(placement.session).unwrap();
        let live = checkout
            .machine()
            .compile_view_in(placement.lexical_scope)
            .is_some();
        let holes = checkout
            .machine()
            .parked_holes()
            .into_iter()
            .map(str::to_owned)
            .collect();
        checkout.restore_suspended(holes);
        live
    }
}

struct CleanupGate {
    entered: tokio::sync::Semaphore,
    release: tokio::sync::Semaphore,
}

impl CleanupGate {
    fn new() -> Self {
        Self {
            entered: tokio::sync::Semaphore::new(0),
            release: tokio::sync::Semaphore::new(0),
        }
    }
}

/// Observe the ownership issued at spawn and require the retirement intent to
/// reach every admitted member before any member begins cleanup.
struct ObservedBehavior {
    inner: Behavior,
    context: Option<tokio::sync::oneshot::Sender<KernelContext>>,
    owner: crate::local_actor::SpawnOwnership,
    peers: Arc<Mutex<Vec<LocalActorRef>>>,
    cleanups: Arc<AtomicUsize>,
    gate: Option<Arc<CleanupGate>>,
    start_gate: Option<Arc<tokio::sync::Semaphore>>,
}

impl KernelBehavior for ObservedBehavior {
    fn start<'a>(
        &'a mut self,
        context: &'a KernelContext,
    ) -> BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>> {
        assert!(match self.owner {
            crate::local_actor::SpawnOwnership::Independent => {
                matches!(
                    context.spawn_ownership(),
                    crate::local_actor::SpawnOwnership::Independent
                )
            }
            crate::local_actor::SpawnOwnership::Supervised(expected) => matches!(
                context.spawn_ownership(),
                crate::local_actor::SpawnOwnership::Supervised(actual) if actual == expected
            ),
        });
        assert!(self.context.take().unwrap().send(context.clone()).is_ok());
        let start_gate = self.start_gate.clone();
        let inner = &mut self.inner;
        Box::pin(async move {
            if let Some(start_gate) = start_gate {
                start_gate.acquire().await.unwrap().forget();
            }
            inner.start(context).await
        })
    }

    fn cast<'a>(
        &'a mut self,
        context: &'a KernelContext,
        sender: ActorRef,
        request: MailboxValue,
    ) -> BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>> {
        self.inner.cast(context, sender, request)
    }

    fn call<'a>(
        &'a mut self,
        context: &'a KernelContext,
        caller: ActorRef,
        ancestry: crate::CallAncestry,
        request: MailboxValue,
    ) -> BoxFuture<'a, Result<KernelStep<MailboxValue>, KernelBehaviorError>> {
        self.inner.call(context, caller, ancestry, request)
    }

    fn tool<'a>(
        &'a mut self,
        context: &'a KernelContext,
        invocation: exomonad_tool::ToolInvocation,
        capture: Option<Arc<dyn crate::HostedCheckpointCapture>>,
    ) -> BoxFuture<'a, Result<KernelStep<serde_json::Value>, KernelInvocationFailure>> {
        self.inner.tool(context, invocation, capture)
    }

    fn workbench<'a>(
        &'a mut self,
        context: &'a KernelContext,
        invocation: crate::ActorWorkbenchInvocation,
        control: Option<Arc<crate::WorkbenchExecutionControl>>,
    ) -> BoxFuture<'a, Result<KernelStep<WorkbenchResponse>, KernelInvocationFailure>> {
        self.inner.workbench(context, invocation, control)
    }

    fn external_application_failed<'a>(
        &'a mut self,
        context: &'a KernelContext,
        failure: ExternalApplicationFailure,
    ) -> BoxFuture<'a, ExternalFailureDisposition> {
        self.inner.external_application_failed(context, failure)
    }

    fn shutdown<'a>(
        &'a mut self,
        context: &'a KernelContext,
        terminal: &'a ActorTerminal,
    ) -> BoxFuture<'a, Result<(), KernelBehaviorError>> {
        self.inner.shutdown(context, terminal)
    }

    fn shutdown_components<'a>(
        &'a mut self,
        context: &'a KernelContext,
        terminal: &'a ActorTerminal,
        deadline: tokio::time::Instant,
    ) -> BoxFuture<
        'a,
        (
            crate::CleanupComponentOutcome,
            crate::CleanupComponentOutcome,
        ),
    > {
        for peer in self.peers.lock().iter() {
            assert!(
                peer.terminal().requested_shutdown().is_some(),
                "cleanup preceded retirement intent for an admitted member"
            );
        }
        self.cleanups.fetch_add(1, Ordering::Relaxed);
        let gate = self.gate.clone();
        let inner = &mut self.inner;
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.entered.add_permits(1);
                gate.release.acquire().await.unwrap().forget();
            }
            inner.shutdown_components(context, terminal, deadline).await
        })
    }

    fn stopped<'a>(
        &'a mut self,
        context: &'a KernelContext,
        terminal: &'a ActorTerminal,
    ) -> BoxFuture<'a, ()> {
        self.inner.stopped(context, terminal)
    }

    fn child_exited(&mut self, notice: ChildExitNotice) {
        self.inner.child_exited(notice);
    }
}

#[tokio::test]
async fn forest_cancels_run_owned_and_supervised_members_before_cleanup() {
    let fixture = Fixture::new().await;
    let forest = &fixture.forest;
    let placement = forest
        .environment
        .runner
        .provision_root_scope(forest.session)
        .await
        .unwrap();
    let peers = Arc::new(Mutex::new(Vec::new()));
    let cleanups = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(CleanupGate::new());
    let (send, receive) = tokio::sync::oneshot::channel();
    let (parent, parent_task) = crate::local_actor::spawn_local_actor_in_directory(
        None,
        ObservedBehavior {
            inner: fixture.behavior(ActorDescriptor::new("observed-root", placement)),
            context: Some(send),
            owner: crate::local_actor::SpawnOwnership::Independent,
            peers: peers.clone(),
            cleanups: cleanups.clone(),
            gate: Some(gate.clone()),
            start_gate: None,
        },
        forest.incarnation,
        forest.directory.clone(),
    )
    .await
    .unwrap();
    let parent_kernel = receive.await.unwrap();

    let child_placement = fixture.child_placement(&parent).await;
    let (send, receive) = tokio::sync::oneshot::channel();
    let child = parent_kernel
        .spawn_child(
            None,
            ObservedBehavior {
                inner: fixture.behavior(
                    ActorDescriptor::new("supervised-child", child_placement)
                        .with_supervisor_parent(Some(parent.identity())),
                ),
                context: Some(send),
                owner: crate::local_actor::SpawnOwnership::Supervised(parent.identity()),
                peers: peers.clone(),
                cleanups: cleanups.clone(),
                gate: Some(gate.clone()),
                start_gate: None,
            },
        )
        .await
        .unwrap();
    let child_kernel = receive.await.unwrap();
    assert!(matches!(
        child_kernel.spawn_ownership(),
        crate::local_actor::SpawnOwnership::Supervised(owner) if owner == parent.identity()
    ));

    let run_owned_placement = forest
        .environment
        .runner
        .provision_root_scope(forest.session)
        .await
        .unwrap();
    let (send, receive) = tokio::sync::oneshot::channel();
    let start_gate = Arc::new(tokio::sync::Semaphore::new(0));
    let spawning_kernel = parent_kernel.clone();
    let run_owned_behavior = ObservedBehavior {
        inner: fixture.behavior(ActorDescriptor::new("run-owned", run_owned_placement)),
        context: Some(send),
        owner: crate::local_actor::SpawnOwnership::Independent,
        peers: peers.clone(),
        cleanups: cleanups.clone(),
        gate: Some(gate.clone()),
        start_gate: Some(start_gate.clone()),
    };
    let spawning_run_owned = tokio::spawn(async move {
        spawning_kernel
            .spawn_worker(None, run_owned_behavior, crate::WorkerLifetime::RunOwned)
            .await
    });
    let run_owned_kernel = receive.await.unwrap();
    let run_owned = run_owned_kernel
        .resolve(run_owned_kernel.identity())
        .await
        .unwrap()
        .expect("pre_start has admitted the actor to the directory");
    assert!(matches!(
        run_owned_kernel.spawn_ownership(),
        crate::local_actor::SpawnOwnership::Independent
    ));

    *peers.lock() = vec![parent.clone(), child.clone(), run_owned.clone()];

    let shutting_down = {
        let forest = forest.clone();
        tokio::spawn(async move { forest.shutdown().await })
    };
    let admitted = peers.lock().clone();
    for actor in &admitted {
        tokio::time::timeout(
            Duration::from_secs(2),
            actor.terminal().wait_requested_shutdown(),
        )
        .await
        .unwrap();
    }
    assert!(
        run_owned.terminal().get().is_none(),
        "the admitted pre_start member has not published an exit"
    );
    start_gate.add_permits(1);
    tokio::time::timeout(Duration::from_secs(2), gate.entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    assert!(
        peers
            .lock()
            .iter()
            .all(|actor| actor.terminal().requested_shutdown().is_some()),
        "the first cleanup must follow intent for the complete captured membership"
    );
    assert!(cleanups.load(Ordering::Relaxed) >= 1);
    gate.release.add_permits(3);
    let outcomes = tokio::time::timeout(Duration::from_secs(5), shutting_down)
        .await
        .unwrap()
        .unwrap();
    assert!(
        outcomes.iter().all(crate::ForestRootShutdown::is_confirmed),
        "actual forest shutdown: {outcomes:#?}"
    );
    let roots = outcomes
        .into_iter()
        .filter_map(|outcome| match outcome {
            crate::ForestRootShutdown::Settled(shutdown) => Some(shutdown.cleanup.actor()),
            crate::ForestRootShutdown::RunResources(_) => None,
            other => panic!("unconfirmed root shutdown: {other:?}"),
        })
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(roots, [parent.identity(), run_owned.identity()].into());
    assert!(child.terminal().cleanup().unwrap().is_confirmed());
    assert_eq!(cleanups.load(Ordering::Relaxed), 3);
    let refused_run_owned = tokio::time::timeout(Duration::from_secs(2), spawning_run_owned)
        .await
        .unwrap()
        .unwrap()
        .expect_err("seal refuses startup before the behavior opens ingress");
    let startup_cleanup = crate::local_actor::startup_cleanup(&refused_run_owned)
        .expect("the admitted producer retains its actual startup cleanup");
    assert_eq!(startup_cleanup.actor(), run_owned.identity());
    assert!(startup_cleanup.is_confirmed());
    tokio::time::timeout(Duration::from_secs(2), parent_task)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn sealed_forest_returns_typed_actual_owner_startup_cleanup() {
    let fixture = Fixture::new().await;
    let forest = &fixture.forest;
    let placement = forest
        .environment
        .runner
        .provision_root_scope(forest.session)
        .await
        .unwrap();
    assert!(fixture.scope_is_live(placement));

    let retirement = forest
        .directory
        .seal()
        .await
        .unwrap()
        .cancel_all(ActorTerminal::new(
            ActorExitKind::Cancelled,
            "forest host shutdown",
        ))
        .await;
    assert!(retirement.into_roots().is_empty());
    let error = match crate::local_actor::spawn_local_actor_in_directory(
        None,
        fixture.behavior(ActorDescriptor::new("staged-root", placement)),
        forest.incarnation,
        forest.directory.clone(),
    )
    .await
    {
        Ok(_) => panic!("sealed directory admitted staged root"),
        Err(error) => error,
    };
    let cleanup = crate::local_actor::startup_cleanup(&error)
        .unwrap_or_else(|| panic!("startup refusal retains cleanup from the placement owner; actual refusal: {error:?}; placement still live: {}", fixture.scope_is_live(placement)));
    assert!(cleanup.is_confirmed());
    assert!(forest
        .directory
        .resolve(cleanup.actor())
        .await
        .unwrap()
        .is_none());
    assert!(forest.environment.actors.lock().is_empty());
    assert!(!fixture.scope_is_live(placement));

    let error = match forest
        .new_workbench("late-workbench".into(), crate::ActorCapabilities::default())
        .await
    {
        Ok(_) => panic!("sealed forest admitted a late workbench"),
        Err(error) => error,
    };
    assert!(
        matches!(
            error.downcast_ref::<crate::local_actor::DirectoryError>(),
            Some(crate::local_actor::DirectoryError::Closed)
        ),
        "late workbench is refused at reservation before native preparation: {error:?}"
    );
    assert!(forest.environment.actors.lock().is_empty());
    assert!(!fixture.scope_is_live(placement));
    let outcomes = forest.shutdown().await;
    assert_eq!(
        outcomes.len(),
        1,
        "sealed prepared shutdown outcomes: {outcomes:?}"
    );
    assert!(matches!(
        outcomes.as_slice(),
        [crate::ForestRootShutdown::RunResources(
            crate::CleanupComponentOutcome::Confirmed
        )]
    ));
}

#[tokio::test]
async fn stopped_lifecycle_owner_still_cleans_prepared_native_input_without_confirming_run() {
    let fixture = Fixture::new().await;
    let forest = &fixture.forest;
    let placement = forest
        .environment
        .runner
        .provision_root_scope(forest.session)
        .await
        .unwrap();
    assert!(fixture.scope_is_live(placement));
    assert!(forest
        .directory
        .seal()
        .await
        .unwrap()
        .cancel_all(ActorTerminal::new(
            ActorExitKind::Cancelled,
            "owner-loss fault control"
        ))
        .await
        .into_roots()
        .is_empty());
    let reservation = forest
        .directory
        .reserve_existing_input(forest.incarnation)
        .await
        .unwrap();
    assert!(reservation.cleanup_only());
    let identity = reservation.identity();
    reservation.lifecycle().stop_owner_for_test().await;
    let error = match crate::local_actor::spawn_reserved_local_actor(
        None,
        fixture.behavior(ActorDescriptor::new("lost-lifecycle", placement)),
        reservation,
        forest.directory.clone(),
    )
    .await
    {
        Ok(_) => panic!("stopped lifecycle owner admitted prepared input"),
        Err(error) => error,
    };
    let cause = match &error {
        ractor::SpawnErr::StartupFailed(error) => error
            .source()
            .and_then(|source| source.downcast_ref::<crate::local_actor::StartupFailureCause>()),
        _ => None,
    };
    assert!(
        matches!(
            cause,
            Some(crate::local_actor::StartupFailureCause::Lifecycle(_))
        ),
        "actual lifecycle refusal is retained: {error:?}"
    );
    let cleanup = crate::local_actor::startup_cleanup(&error)
        .expect("actual prepared placement cleanup survives lifecycle owner loss");
    assert_eq!(cleanup.actor(), identity);
    assert!(matches!(
        cleanup.hook(),
        crate::CleanupComponentOutcome::Confirmed
    ));
    assert!(matches!(
        cleanup.realm(),
        crate::CleanupComponentOutcome::Confirmed
    ));
    assert!(matches!(
        cleanup.children(),
        crate::CleanupComponentOutcome::Unconfirmed(_)
    ));
    assert!(!cleanup.is_confirmed());
    assert!(!fixture.scope_is_live(placement));
    assert!(forest.directory.resolve(identity).await.unwrap().is_none());
    assert!(matches!(
        forest
            .directory
            .settle_preparations(Duration::from_secs(2))
            .await,
        crate::CleanupComponentOutcome::Confirmed
    ));
    assert!(matches!(
        forest.directory.shutdown_run_resources().await,
        crate::CleanupComponentOutcome::Confirmed
    ));
    assert!(
        forest.directory.finish_run().await.is_err(),
        "physical coordinator stop alone cannot certify run finalization"
    );
}

#[tokio::test]
async fn refused_invocation_child_retains_physical_cleanup_before_run_finalization() {
    let fixture = Fixture::new().await;
    let forest = &fixture.forest;
    let parent = forest
        .new_workbench(
            "invocation-parent".into(),
            crate::ActorCapabilities::default(),
        )
        .await
        .unwrap();
    let kernel = forest
        .directory
        .context(parent.identity())
        .await
        .unwrap()
        .unwrap();
    let work =
        invocation_work::InvocationWork::new(parent.identity(), RequestReservationOwner::Scope(0));
    assert!(work
        .cleanup(&forest.environment, &kernel)
        .await
        .uncertainty()
        .is_none());
    let placement = forest
        .environment
        .runner
        .provision_root_scope(forest.session)
        .await
        .unwrap();
    assert!(fixture.scope_is_live(placement));
    let error = match kernel
        .spawn_worker_scoped(
            None,
            fixture.behavior(ActorDescriptor::new("closed-invocation-child", placement)),
            crate::WorkerLifetime::RunOwned,
            work,
        )
        .await
    {
        Ok(_) => panic!("closed actual invocation ledger admitted a child"),
        Err(error) => error,
    };
    let cause = match &error {
        ractor::SpawnErr::StartupFailed(error) => error
            .source()
            .and_then(|source| source.downcast_ref::<crate::local_actor::StartupFailureCause>()),
        _ => None,
    };
    assert!(
        matches!(
            cause,
            Some(crate::local_actor::StartupFailureCause::Reservation(_))
        ),
        "production invocation owner refusal retained: {error:?}"
    );
    let cleanup =
        crate::local_actor::startup_cleanup(&error).expect("actual child startup cleanup retained");
    assert!(cleanup.is_confirmed());
    assert!(!fixture.scope_is_live(placement));
    assert!(forest
        .directory
        .resolve(cleanup.actor())
        .await
        .unwrap()
        .is_none());
    let outcomes = forest.shutdown().await;
    assert!(
        outcomes.iter().all(crate::ForestRootShutdown::is_confirmed),
        "actual refused child finalization outcomes: {outcomes:?}"
    );
}

#[tokio::test]
async fn seal_overtaking_issued_worker_still_retains_actual_cleanup_executor() {
    let fixture = Fixture::new().await;
    let forest = &fixture.forest;
    let parent = forest
        .new_workbench("seal-parent".into(), crate::ActorCapabilities::default())
        .await
        .unwrap();
    let kernel = forest
        .directory
        .context(parent.identity())
        .await
        .unwrap()
        .unwrap();
    let reservation = kernel
        .reserve_worker_creation(crate::WorkerLifetime::RunOwned)
        .await
        .unwrap();
    let identity = reservation.identity();
    let placement = forest
        .environment
        .runner
        .provision_root_scope(forest.session)
        .await
        .unwrap();
    assert!(fixture.scope_is_live(placement));
    forest.directory.seal().await.unwrap();
    let error = match crate::local_actor::spawn_reserved_local_actor(
        None,
        fixture.behavior(ActorDescriptor::new("issued-before-seal", placement)),
        reservation,
        forest.directory.clone(),
    )
    .await
    {
        Ok(_) => panic!("sealed run published an issued worker"),
        Err(error) => error,
    };
    let cause = match &error {
        ractor::SpawnErr::StartupFailed(error) => error
            .source()
            .and_then(|source| source.downcast_ref::<crate::local_actor::StartupFailureCause>()),
        _ => None,
    };
    assert!(
        matches!(cause, Some(crate::local_actor::StartupFailureCause::Lifecycle(crate::KernelCallFailure::MailboxClosed(actor))) if *actor == identity),
        "actual closed lifecycle attachment refused: {error:?}"
    );
    let cleanup = crate::local_actor::startup_cleanup(&error)
        .expect("actual prepared worker cleanup retained");
    assert_eq!(cleanup.actor(), identity);
    assert!(cleanup.is_confirmed());
    assert!(!fixture.scope_is_live(placement));
    assert!(forest.directory.resolve(identity).await.unwrap().is_none());
    let outcomes = forest.shutdown().await;
    assert!(
        outcomes.iter().all(crate::ForestRootShutdown::is_confirmed),
        "issued worker cleanup/finalization: {outcomes:?}"
    );
}

#[tokio::test]
async fn tool_abort_validates_original_owner_before_empty_settlement() {
    use tidepool_runtime::session::{
        ContextCheckpointBoundary, WorkbenchExecutionId, WorkbenchRequest,
    };

    let fixture = Fixture::new().await;
    let forest = &fixture.forest;
    let placement = forest
        .environment
        .runner
        .provision_root_scope(forest.session)
        .await
        .unwrap();
    let peers = Arc::new(Mutex::new(Vec::new()));
    let cleanups = Arc::new(AtomicUsize::new(0));
    let (send, receive) = tokio::sync::oneshot::channel();
    let (actor, task) = crate::local_actor::spawn_local_actor_in_directory(
        None,
        ObservedBehavior {
            inner: fixture.behavior(ActorDescriptor::new("abort-owner", placement)),
            context: Some(send),
            owner: crate::local_actor::SpawnOwnership::Independent,
            peers: peers.clone(),
            cleanups: cleanups.clone(),
            gate: None,
            start_gate: None,
        },
        forest.incarnation,
        forest.directory.clone(),
    )
    .await
    .unwrap();
    let kernel = receive.await.unwrap();
    let boundary =
        ContextCheckpointBoundary::external("thread".into(), "turn".into(), "call".into());
    let execution = WorkbenchExecutionId::from_digest([41; 16]);
    let request = WorkbenchRequest::from_cell_input("pure ()")
        .with_execution_id(execution.clone())
        .with_checkpoint_boundary(boundary.clone());
    let mut behavior = fixture.behavior(ActorDescriptor::new("abort-owner", placement));

    let nested_only = crate::resident_tools::WorkbenchCallKey::from(
        exomonad_tool::ToolInvocationContext::external(
            "thread".into(),
            "turn".into(),
            "inner".into(),
            Some("call".into()),
            None,
        ),
    );
    behavior
        .workbench_executions
        .lock()
        .begin(&execution, request.clone(), Some(&nested_only));
    let error = behavior
        .tool_aborted(&kernel, boundary.clone())
        .await
        .expect_err("a nested cell cannot settle its original provider operation");
    assert!(!behavior.settled_checkpoint_boundaries.contains(&boundary));
    assert!(error.detail.contains("original") || error.detail.contains("exact"));

    let invocation =
        exomonad_tool::ToolInvocationContext::original(boundary.hosted().unwrap().clone());
    let original = crate::resident_tools::WorkbenchCallKey::from(invocation.clone());
    behavior
        .workbench_executions
        .lock()
        .begin(&execution, request, Some(&original));
    behavior
        .tool_aborted(&kernel, boundary.clone())
        .await
        .expect_err("adding an original entry does not certify multiple owners");
    assert!(!behavior.settled_checkpoint_boundaries.contains(&boundary));

    // A separate admitted operation owns a fresh journal; the refused nested
    // and ambiguous journals remain intact.
    let mut original_behavior = fixture.behavior(ActorDescriptor::new("abort-owner", placement));
    let control = crate::WorkbenchExecutionControl::from_invocation(Some(invocation));
    let execution = control.execution_id(kernel.identity());
    let request = WorkbenchRequest::from_cell_input("pure ()")
        .with_execution_id(execution.clone())
        .with_checkpoint_boundary(boundary.clone());
    actor
        .hosted_cell()
        .publish_provider_transport(kernel.identity(), control.clone());
    actor.hosted_cell().accept(&control);
    {
        let mut journal = original_behavior.workbench_executions.lock();
        journal.begin(&execution, request, Some(&original));
        journal.bind_provider_finalization(&execution, Some(&original), Some(&control));
    }
    let owner = actor.hosted_cell().retained_boundary(&boundary).unwrap();
    assert!(matches!(
        owner.finalization(),
        crate::HostedOperationFinalization::Pending
    ));
    original_behavior
        .tool_aborted(&kernel, boundary.clone())
        .await
        .expect("the issued original owner can settle an empty abort after validation");
    assert!(original_behavior
        .settled_checkpoint_boundaries
        .contains(&boundary));
    assert!(matches!(
        owner.finalization(),
        crate::HostedOperationFinalization::Settled(Ok(crate::ProviderFinalizationKind::Aborted))
    ));
    assert!(!behavior.settled_checkpoint_boundaries.contains(&boundary));

    let shutdown = forest.shutdown().await;
    assert!(shutdown.iter().all(crate::ForestRootShutdown::is_confirmed));
    assert!(actor.terminal().cleanup().unwrap().is_confirmed());
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn native_mailbox_casts_retain_recipient_compiler_close_after_caller_retirement() {
    native_mailbox_compiler_lifecycle_history().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_mailbox_compiler_lifecycle_on_tokio_workers() {
    native_mailbox_compiler_lifecycle_history().await;
}

async fn native_mailbox_compiler_lifecycle_history() {
    use crate::resident_workbench::CompilerCloseOwner;
    use std::path::PathBuf;
    use tidepool_codegen::scope::ScopeId;
    use tidepool_runtime::session::{
        insert_preamble_imports, resident_workbench_templates, turn::run_turn, PersistentSession,
        TurnRequest, TurnResult,
    };

    tidepool_testing::eval_harness::require_extract();
    let surface = tidepool_testing::effect_surface::TestEffectSurface::minimal(&[
        tidepool_mcp::actor_local_decl(),
        tidepool_mcp::actor_kernel_decl(),
        tidepool_mcp::console_decl(),
    ])
    .unwrap();
    let root = tempfile::tempdir().unwrap();
    let session_id = tidepool_runtime::session::fresh_session_id();
    let lib = SessionLib::open(session_id, root.path(), ModuleEnv::standalone_default())
        .unwrap()
        .with_validation_include(surface.include_paths().to_vec());
    let persistent = PersistentSession::new(Some(lib), tidepool_runtime::DEFAULT_NURSERY_SIZE);
    let view = persistent.compile_view_in(ScopeId::ROOT).unwrap();
    let preamble = insert_preamble_imports(
        surface.preamble(),
        "qualified Tidepool.Actor as Actor\nqualified Tidepool.Effects.Core as Core\nqualified Data.Text as Text",
    );
    let templates = resident_workbench_templates(
        &preamble,
        "'[Core.ActorKernel, Core.ActorLocal Maybe, Core.Console]",
        "",
    );
    let include = view.include_paths(surface.include_paths());
    let paths = include.iter().map(PathBuf::as_path).collect::<Vec<_>>();
    let compile = |source: &str, gen| {
        tidepool_testing::with_settlement(|settlement| {
            run_turn(
                TurnRequest {
                    exact_context: None,
                    session_id: None,
                    turn_text: source,
                    templates: &templates,
                    include: &paths,
                    session_root: view.session_root(),
                    inject_modules: &[],
                    gen,
                    verdict: None,
                    target: None,
                    retained_imports: &[],
                },
                settlement,
            )
        })
        .unwrap()
    };
    let TurnResult::Bind {
        compiled: values,
        bound,
        ..
    } = compile(
        "(mailboxCast, mailboxQuery) <- pure (Just (), Just (42 :: Int))",
        1,
    )
    else {
        panic!("native mailbox payload bindings")
    };
    let TurnResult::Bind {
        compiled: receiver,
        bound: receiver_bound,
        ..
    } = compile(
        r#"mailboxReceiver <- pure ((\(_ :: Int) -> do
  send (Core.ActorInstallShutdownWith 0 (\reason -> (send (Core.Print (Text.pack (show (reason :: Int)))) :: Eff '[Core.Console] ())))
  send Core.ActorReadyWith
  Actor.serve @() @Maybe () (\() request -> case request of Just value -> pure (value, ()); Nothing -> error "unused mailbox request")
  ) :: Int -> Eff '[Core.ActorKernel, Core.ActorLocal Maybe, Core.Console] ())"#,
        2,
    )
    else {
        panic!("native receiver loop and shutdown hook")
    };
    let [receiver_binder] = receiver_bound.as_slice() else {
        panic!("one retained native receiver entry")
    };
    for (kind, reason_code) in [
        (ActorExitKind::Completed, "0"),
        (ActorExitKind::Failed, "1"),
        (ActorExitKind::Cancelled, "2"),
    ] {
        let machine_root = tempfile::tempdir().unwrap();
        let session_id = tidepool_runtime::session::fresh_session_id();
        let lib = SessionLib::open(
            session_id,
            machine_root.path(),
            ModuleEnv::standalone_default(),
        )
        .unwrap()
        .with_validation_include(include.clone());
        let hook_calls = Arc::new(Mutex::new(Vec::new()));
        let target = Arc::new(Mutex::new(None));
        let mut session = ResidentSession::unbootstrapped(
            NativeShutdownProbe {
                target: target.clone(),
                calls: hook_calls.clone(),
            },
            tidepool_mcp::CapturedOutput::new(),
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            Some(lib),
        );
        tidepool_testing::with_settlement(|settlement| {
            session.run_projected_bind_with_sites(
                "native-mailbox-values",
                values.code(),
                &bound,
                tidepool_repr::Generation(1),
                settlement,
            )
        })
        .unwrap();
        let casts = (0..3)
            .map(|_| {
                session
                    .retain_binding_custody("mailboxCast")
                    .unwrap()
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let query = session
            .retain_binding_custody("mailboxQuery")
            .unwrap()
            .unwrap();
        let placement = crate::ActorPlacement {
            session: session_id,
            resource_scope: RealmId::fresh(),
            lexical_scope: session.mint_isolated_scope(),
        };
        tidepool_testing::with_settlement(|settlement| {
            session.run_bind_with_sites(
                "native-mailbox-receiver",
                receiver.code(),
                receiver_binder,
                tidepool_repr::Generation(2),
                settlement,
            )
        })
        .unwrap();
        let entry = session
            .retain_binding_custody("mailboxReceiver")
            .unwrap()
            .unwrap();
        let (forest, _deployments) = ResidentForest::new(
            ActorWorkbenchSource::new(preamble.clone(), include.clone()),
            session_id,
            session,
            None,
            crate::Incarnation::FIRST,
        )
        .await
        .unwrap();
        let (recipient, recipient_task) = crate::local_actor::spawn_local_actor_in_directory(
            None,
            ResidentKernelBehavior::with_boot(
                ActorDescriptor::new("native-mailbox-recipient", placement),
                forest.environment.clone(),
                ResidentBoot::Entry(entry),
                Vec::new(),
            ),
            forest.incarnation,
            forest.directory.clone(),
        )
        .await
        .unwrap();
        *target.lock() = Some(recipient.terminal().clone());
        let caller_placement = forest
            .environment
            .runner
            .provision_root_scope(session_id)
            .await
            .unwrap();
        let (caller, caller_task) = crate::local_actor::spawn_local_actor_in_directory(
            None,
            ResidentKernelBehavior::with_boot(
                ActorDescriptor::new("completed-mailbox-caller", caller_placement),
                forest.environment.clone(),
                ResidentBoot::Workbench,
                Vec::new(),
            ),
            forest.incarnation,
            forest.directory.clone(),
        )
        .await
        .unwrap();
        caller
            .shutdown(ActorTerminal::new(
                ActorExitKind::Completed,
                "caller finished",
            ))
            .await
            .unwrap();
        caller_task.await.unwrap();
        let caller_closes = caller.terminal().compiler_close_observations();
        assert!(CompilerCloseOwner::current().is_err());
        let machines = forest.environment.runner.machines_for_test();
        let mut observed = recipient.terminal().compiler_close_observations().len();
        for custody in casts {
            recipient
                .cast(caller.identity(), MailboxValue::new(session_id, custody))
                .await
                .unwrap();
            recipient
                .address()
                .call(
                    |reply| KernelMessage::SealHostedWork { reply },
                    Some(Duration::from_secs(30)),
                )
                .await
                .unwrap()
                .unwrap();
            let closes = recipient.terminal().compiler_close_observations();
            assert!(
                closes.len() > observed,
                "each native cast issues recipient-owned compiler work"
            );
            assert!(
                closes
                    .iter()
                    .all(crate::termination::CompilerWorkClose::is_confirmed),
                "{closes:?}"
            );
            observed = closes.len();
            assert!(CompilerCloseOwner::current().is_err());
        }
        let reply = tokio::time::timeout(
            Duration::from_secs(30),
            recipient.call(
                caller.identity(),
                crate::CallAncestry::begin(caller.identity()),
                MailboxValue::new(session_id, query),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        let mut checkout = machines.checkout_run(session_id).unwrap();
        assert!(checkout
            .machine()
            .render_retained_preview(&reply.into_custody(), 64)
            .unwrap()
            .contains("42"));
        let holes = checkout
            .machine()
            .parked_holes()
            .into_iter()
            .map(str::to_owned)
            .collect();
        checkout.restore_suspended(holes);
        assert_eq!(
            caller.terminal().compiler_close_observations(),
            caller_closes
        );
        let before_shutdown = recipient.terminal().compiler_close_observations().len();
        assert!(hook_calls.lock().is_empty());
        recipient
            .shutdown(ActorTerminal::new(kind, "native mailbox checked"))
            .await
            .unwrap();
        recipient_task.await.unwrap();
        let closes = recipient.terminal().compiler_close_observations();
        assert!(
            closes.len() > before_shutdown,
            "native hook owns an additional finalization receipt"
        );
        assert_eq!(
            hook_calls.lock().as_slice(),
            &[(kind, reason_code.to_owned())]
        );
        assert!(recipient.terminal().cleanup().unwrap().is_confirmed());
        assert!(
            closes
                .iter()
                .all(crate::termination::CompilerWorkClose::is_confirmed),
            "{closes:?}"
        );
        assert!(CompilerCloseOwner::current().is_err());
    }
}

#[derive(tidepool_bridge_derive::FromHaskell)]
enum NativeShutdownPrint {
    Print(String),
}

struct NativeShutdownProbe {
    target: Arc<Mutex<Option<crate::RetainedActorExit>>>,
    calls: Arc<Mutex<Vec<(ActorExitKind, String)>>>,
}

impl tidepool_effect::dispatch::DispatchEffect<tidepool_mcp::CapturedOutput>
    for NativeShutdownProbe
{
    fn dispatch(
        &mut self,
        request: &tidepool_bridge::HaskellValue,
        context: &tidepool_effect::dispatch::EffectContext<'_, tidepool_mcp::CapturedOutput>,
    ) -> Result<Option<tidepool_effect::Response>, tidepool_effect::EffectError> {
        use tidepool_bridge::FromHaskell;
        let Ok(NativeShutdownPrint::Print(reason)) =
            NativeShutdownPrint::from_value(request, context.table())
        else {
            return Ok(None);
        };
        let target = self
            .target
            .lock()
            .clone()
            .expect("installed hook belongs to admitted recipient");
        let requested = target
            .requested_shutdown()
            .expect("native hook runs after stop was requested");
        self.calls.lock().push((requested.kind, reason));
        Ok(Some(context.respond(())?))
    }

    fn prepare_dispatch(
        &mut self,
        request: &tidepool_bridge::HaskellValue,
        context: &tidepool_effect::dispatch::EffectContext<'_, tidepool_mcp::CapturedOutput>,
    ) -> Result<tidepool_effect::dispatch::EffectDispatch, tidepool_effect::EffectError> {
        Ok(match self.dispatch(request, context)? {
            Some(response) => tidepool_effect::dispatch::EffectDispatch::Immediate(response),
            None => tidepool_effect::dispatch::EffectDispatch::Unhandled,
        })
    }
}

#[tokio::test]
async fn host_graph_retains_paused_owner_lifecycle_and_recorded_exit() {
    let fixture = Fixture::new().await;
    let forest = &fixture.forest;
    let placement = forest
        .environment
        .runner
        .provision_root_scope(forest.session)
        .await
        .unwrap();
    let (actor, task) = crate::local_actor::spawn_local_actor_in_directory(
        None,
        fixture.behavior(ActorDescriptor::new("observed-lifecycle", placement)),
        forest.incarnation,
        forest.directory.clone(),
    )
    .await
    .unwrap();
    let identity = actor.identity();
    let snapshot = || async {
        forest
            .inspect_host_graph()
            .await
            .unwrap()
            .into_iter()
            .find(|node| node.actor == identity)
            .unwrap()
    };
    assert_eq!(
        snapshot().await.lifecycle,
        Some(crate::ActorLifecycle::Live)
    );
    // The graph reads the same retained owner used by failed handler publication.
    actor
        .terminal()
        .publish_paused("retained handler input".into());
    let paused = snapshot().await;
    assert_eq!(
        paused.lifecycle,
        Some(crate::ActorLifecycle::Paused(
            "retained handler input".into()
        ))
    );
    assert!(paused.terminal().is_none());
    let encoded = serde_json::to_value(&paused).unwrap();
    assert_eq!(encoded["lifecycle"]["Paused"], "retained handler input");
    assert!(encoded.get("terminal").is_none());
    let terminal = ActorTerminal::new(ActorExitKind::Cancelled, "test finished");
    actor.shutdown(terminal.clone()).await.unwrap();
    task.await.unwrap();
    let exited = snapshot().await;
    assert_eq!(
        exited.lifecycle,
        Some(crate::ActorLifecycle::Exited(terminal.clone()))
    );
    assert_eq!(exited.terminal(), Some(&terminal));
    let (empty_directory, empty_control_task) = crate::LocalActorDirectory::start().await.unwrap();
    let without_directory = Forest {
        environment: forest.environment.clone(),
        directory: empty_directory,
        _control_task: Arc::new(empty_control_task),
        session: forest.session,
        incarnation: forest.incarnation,
    };
    assert_eq!(
        without_directory.inspect_host_graph().await.unwrap()[0].terminal(),
        Some(&terminal)
    );
    forest
        .environment
        .actors
        .lock()
        .get_mut(&identity)
        .unwrap()
        .terminal = None;
    assert_eq!(
        without_directory.inspect_host_graph().await.unwrap()[0].lifecycle,
        None
    );
    without_directory.directory.stop_owner_for_test().await;
    assert!(matches!(
        without_directory.inspect_host_graph().await,
        Err(crate::DirectoryError::Unavailable)
    ));
    forest
        .environment
        .actors
        .lock()
        .get_mut(&identity)
        .unwrap()
        .terminal = actor.terminal().get();
    let retired = without_directory.inspect_host_graph().await.unwrap();
    assert_eq!(retired.len(), 1);
    assert_eq!(retired[0].actor, identity);
    assert_eq!(retired[0].terminal(), Some(&terminal));
    assert!(retired[0].active_requests.is_empty());
    assert!(retired[0].queued_requests.is_empty());
}
