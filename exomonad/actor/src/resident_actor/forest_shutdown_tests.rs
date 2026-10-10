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

#[tokio::test]
async fn native_mailbox_casts_retain_recipient_compiler_close_after_caller_retirement() {
    use crate::resident_workbench::CompilerCloseOwner;
    use std::path::PathBuf;
    use tidepool_codegen::scope::ScopeId;
    use tidepool_runtime::session::{
        insert_preamble_imports, resident_workbench_templates, turn::run_turn, PersistentSession,
        SessionRunContext, TurnRequest, TurnResult,
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
    let TurnResult::Expr {
        compiled: receiver, ..
    } = compile(
        r#"(do
  send (Core.ActorInstallShutdownWith 0 (\reason -> send (Core.Print (Text.pack (show (reason :: Int))))))
  Actor.serve @() @Maybe () (\() request -> case request of Just value -> pure (value, ()); Nothing -> error "unused mailbox request")
  ) :: Eff '[Core.ActorKernel, Core.ActorLocal Maybe, Core.Console] ()"#,
        2,
    )
    else {
        panic!("native receiver loop and shutdown hook")
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
        session
            .set_actor_execution(
                SessionRunContext {
                    lexical_scope: placement.lexical_scope,
                    resource_scope: placement.resource_scope,
                    ..SessionRunContext::ROOT
                },
                tidepool_effect::EffectRunPolicy::HandleOrSuspend,
                tidepool_effect::LivePayloadPolicy::HASKELL_EFFECT_VALUE,
            )
            .unwrap();
        let prepared = tidepool_testing::with_settlement(|settlement| {
            session.run_with_sites("native-mailbox-receiver", receiver.code(), settlement)
        })
        .unwrap();
        let (forest, _deployments) = ResidentForest::new(
            ActorWorkbenchSource::new(preamble.clone(), include.clone()),
            session_id,
            session,
            None,
            crate::Incarnation::FIRST,
        );
        let (recipient, recipient_task) = forest
            .admit_root(
                ActorDescriptor::new("native-mailbox-recipient", placement),
                prepared,
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
