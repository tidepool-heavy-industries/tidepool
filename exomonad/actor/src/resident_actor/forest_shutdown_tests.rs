use super::*;
use futures_util::future::BoxFuture;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tidepool_runtime::session::{ModuleEnv, SessionLib};

type Forest = ResidentForest<frunk::HNil, tidepool_mcp::CapturedOutput>;
type Behavior = ResidentKernelBehavior<frunk::HNil, tidepool_mcp::CapturedOutput>;

struct Fixture {
    forest: Arc<Forest>,
    _deployments: mpsc::Receiver<LocalResidentDeployment>,
    _session_root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let session_root = tempfile::tempdir().unwrap();
        let session_id = tidepool_runtime::session::fresh_session_id();
        let lib = SessionLib::open(
            session_id,
            session_root.path(),
            ModuleEnv::standalone_default(),
        )
        .unwrap();
        let session = ResidentSession::unbootstrapped(
            frunk::HNil,
            tidepool_mcp::CapturedOutput::new(),
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            Some(lib),
        );
        let (forest, deployments) = Forest::new(
            ActorWorkbenchSource::new("", Vec::new()),
            session_id,
            session,
            None,
            crate::Incarnation::FIRST,
        );
        Self {
            forest: Arc::new(forest),
            _deployments: deployments,
            _session_root: session_root,
        }
    }

    fn behavior(&self, descriptor: ActorDescriptor) -> Behavior {
        Behavior::with_boot(
            descriptor,
            self.forest.environment.clone(),
            ResidentBoot::Workbench,
            Vec::new(),
        )
    }

    fn child_placement(&self, parent: &LocalActorRef) -> crate::ActorPlacement {
        let context = self
            .forest
            .directory
            .session_context(parent.identity())
            .unwrap();
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
    let fixture = Fixture::new();
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

    let child_placement = fixture.child_placement(&parent);
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
    assert!(outcomes.iter().all(crate::ForestRootShutdown::is_confirmed));
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
    let spawned_run_owned = tokio::time::timeout(Duration::from_secs(2), spawning_run_owned)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(spawned_run_owned.identity(), run_owned.identity());
    tokio::time::timeout(Duration::from_secs(2), parent_task)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn sealed_forest_returns_typed_actual_owner_startup_cleanup() {
    let fixture = Fixture::new();
    let forest = &fixture.forest;
    let placement = forest
        .environment
        .runner
        .provision_root_scope(forest.session)
        .await
        .unwrap();
    assert!(fixture.scope_is_live(placement));

    let retirement = forest.directory.seal().cancel_all(ActorTerminal::new(
        ActorExitKind::Cancelled,
        "forest host shutdown",
    ));
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
        .expect("startup refusal retains cleanup from the placement owner");
    assert!(cleanup.is_confirmed());
    assert!(forest.directory.resolve(cleanup.actor()).is_none());
    assert!(forest.environment.actors.lock().is_empty());
    assert!(!fixture.scope_is_live(placement));

    let error = match forest
        .new_workbench("late-workbench".into(), crate::ActorCapabilities::default())
        .await
    {
        Ok(_) => panic!("sealed forest admitted a late workbench"),
        Err(error) => error,
    };
    let error = error
        .downcast_ref::<ractor::SpawnErr>()
        .expect("preserve the typed startup refusal");
    assert!(crate::local_actor::startup_cleanup(error)
        .expect("the actual startup owner cleaned its placement")
        .is_confirmed());
    let outcomes = forest.shutdown().await;
    assert_eq!(outcomes.len(), 1);
    assert!(matches!(
        outcomes.as_slice(),
        [crate::ForestRootShutdown::RunResources(
            crate::CleanupComponentOutcome::Confirmed
        )]
    ));
}

#[tokio::test]
async fn tool_abort_validates_original_owner_before_empty_settlement() {
    use tidepool_runtime::session::{
        ContextCheckpointBoundary, WorkbenchExecutionId, WorkbenchRequest,
    };

    let fixture = Fixture::new();
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

    let original = crate::resident_tools::WorkbenchCallKey::from(
        exomonad_tool::ToolInvocationContext::external(
            "thread".into(),
            "turn".into(),
            "call".into(),
            Some("call".into()),
            None,
        ),
    );
    behavior
        .workbench_executions
        .lock()
        .begin(&execution, request, Some(&original));
    behavior
        .tool_aborted(&kernel, boundary.clone())
        .await
        .expect("the original owner can settle an empty abort after validation");
    assert!(behavior.settled_checkpoint_boundaries.contains(&boundary));

    let shutdown = forest.shutdown().await;
    assert!(shutdown.iter().all(crate::ForestRootShutdown::is_confirmed));
    assert!(actor.terminal().cleanup().unwrap().is_confirmed());
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
}
