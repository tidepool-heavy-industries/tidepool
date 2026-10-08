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
        let session_id = tidepool_repr::SessionId(79);
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
            crate::Incarnation(1),
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

/// Observe the real resident initializer and cleanup without supplying either
/// membership records or cleanup evidence from the test.
struct ObservedBehavior {
    inner: Behavior,
    context: Option<tokio::sync::oneshot::Sender<KernelContext>>,
    independent: bool,
    peers: Arc<Mutex<Vec<LocalActorRef>>>,
    cleanups: Arc<AtomicUsize>,
}

impl KernelBehavior for ObservedBehavior {
    fn start<'a>(
        &'a mut self,
        context: &'a KernelContext,
    ) -> BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>> {
        assert_eq!(context.spawn_ownership().is_independent(), self.independent);
        assert_eq!(
            context.supervisor_identity(),
            None,
            "Ractor links after pre_start"
        );
        assert!(self.context.take().unwrap().send(context.clone()).is_ok());
        self.inner.start(context)
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
                "cleanup preceded peer cancellation"
            );
        }
        self.cleanups.fetch_add(1, Ordering::Relaxed);
        self.inner.shutdown_components(context, terminal, deadline)
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
async fn forest_cancels_linked_members_before_staging_and_drains_only_roots() {
    let fixture = Fixture::new();
    let forest = &fixture.forest;
    let workbench = forest
        .new_workbench("host-workbench".into(), crate::EffectiveRole::root())
        .await
        .unwrap();
    let placement = forest
        .environment
        .runner
        .provision_root_scope(forest.session)
        .await
        .unwrap();
    let peers = Arc::new(Mutex::new(Vec::new()));
    let cleanups = Arc::new(AtomicUsize::new(0));
    let (send, receive) = tokio::sync::oneshot::channel();
    let (parent, task) = crate::local_actor::spawn_local_actor_in_directory(
        None,
        ObservedBehavior {
            inner: fixture.behavior(ActorDescriptor::new("observed-root", placement)),
            context: Some(send),
            independent: true,
            peers: peers.clone(),
            cleanups: cleanups.clone(),
        },
        forest.incarnation,
        forest.directory.clone(),
    )
    .await
    .unwrap();
    let kernel = receive.await.unwrap();
    let (send, receive) = tokio::sync::oneshot::channel();
    let child = kernel
        .spawn_child(
            None,
            ObservedBehavior {
                inner: fixture.behavior(
                    ActorDescriptor::new("linked-child", fixture.child_placement(&parent))
                        .with_supervisor_parent(Some(parent.identity())),
                ),
                context: Some(send),
                independent: false,
                peers: peers.clone(),
                cleanups: cleanups.clone(),
            },
        )
        .await
        .unwrap();
    let child_kernel = receive.await.unwrap();
    assert_eq!(child_kernel.supervisor_identity(), Some(parent.identity()));
    {
        let records = forest.environment.actors.lock();
        assert!(records[&workbench.identity()].scheduler_root);
        assert!(records[&parent.identity()].scheduler_root);
        assert!(!records[&child.identity()].scheduler_root);
    }
    *peers.lock() = vec![workbench.clone(), parent.clone(), child.clone()];
    let staging = forest.environment.root_admission_closed.read().await;
    let shutting_down = {
        let forest = forest.clone();
        tokio::spawn(async move { forest.shutdown().await })
    };
    let admitted = peers.lock().clone();
    for actor in admitted {
        tokio::time::timeout(
            Duration::from_secs(2),
            actor.terminal().wait_requested_shutdown(),
        )
        .await
        .unwrap();
    }
    assert!(!shutting_down.is_finished(), "staging is still owned");
    assert_eq!(cleanups.load(Ordering::Relaxed), 0);
    drop(staging);
    let outcomes = tokio::time::timeout(Duration::from_secs(5), shutting_down)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(outcomes.len(), 2);
    assert!(outcomes.iter().all(crate::ForestRootShutdown::is_confirmed));
    let mut roots = outcomes
        .into_iter()
        .map(|outcome| match outcome {
            crate::ForestRootShutdown::Settled(shutdown) => shutdown.cleanup.actor(),
            other => panic!("unconfirmed root shutdown: {other:?}"),
        })
        .collect::<Vec<_>>();
    roots.sort_by_key(|actor| actor.id);
    let mut expected = vec![workbench.identity(), parent.identity()];
    expected.sort_by_key(|actor| actor.id);
    assert_eq!(roots, expected);
    assert_eq!(cleanups.load(Ordering::Relaxed), 2);
    assert!(child.terminal().cleanup().unwrap().is_confirmed());
    assert!(!fixture.scope_is_live(placement));
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn sealed_forest_refuses_provisioned_workbench_with_actual_startup_cleanup() {
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
        Ok(_) => panic!("closed directory admitted staged root"),
        Err(error) => error,
    };
    let cleanup = crate::local_actor::startup_cleanup(&error)
        .expect("pre_start refusal retains actual cleanup");
    assert!(cleanup.is_confirmed());
    assert!(forest.directory.resolve(cleanup.actor()).is_none());
    assert!(forest.environment.actors.lock().is_empty());
    assert!(!fixture.scope_is_live(placement));

    let error = match forest
        .new_workbench("late-workbench".into(), crate::EffectiveRole::root())
        .await
    {
        Ok(_) => panic!("closed directory admitted new_workbench"),
        Err(error) => error,
    };
    let error = error
        .downcast_ref::<ractor::SpawnErr>()
        .expect("original typed startup refusal");
    assert!(crate::local_actor::startup_cleanup(error)
        .unwrap()
        .is_confirmed());
    assert!(forest.shutdown().await.is_empty());
}
