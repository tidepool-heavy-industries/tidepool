//! Public lexical scopes through the production resident workbench. The only
//! controlled service is the external command backend, not ownership or scope
//! execution. Idle tool installation is acknowledged without a provider round.

use super::*;
use crate::command_jobs::{CommandBackend, CommandControl};
use crate::request::{RequestCleanupState, ResourceCleanupOwner, ResponseFailure};
use futures_util::future::BoxFuture;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tidepool_bridge_effects::{
    CommandCleanup, CommandError, CommandOutcome, CommandOutput, CommandPage, CommandPosition,
    CommandResult, CommandSpec, CommandStatus, CommandStream,
};
use tidepool_runtime::session::{insert_preamble_imports, ModuleEnv, SessionLib};
use tidepool_testing::eval_harness;

struct ControlledBackend {
    executions: AtomicUsize,
    cancellations: AtomicUsize,
    cleanup_probes: AtomicUsize,
    started: tokio::sync::Semaphore,
    finish: tokio::sync::Semaphore,
    initial_cleanup_unknown: bool,
    job: Mutex<Option<String>>,
}

impl ControlledBackend {
    fn new(initial_cleanup_unknown: bool) -> Arc<Self> {
        Arc::new(Self {
            executions: AtomicUsize::new(0),
            cancellations: AtomicUsize::new(0),
            cleanup_probes: AtomicUsize::new(0),
            started: tokio::sync::Semaphore::new(0),
            finish: tokio::sync::Semaphore::new(0),
            initial_cleanup_unknown,
            job: Mutex::new(None),
        })
    }

    fn assert_executed_once(&self) {
        assert_eq!(self.executions.load(Ordering::SeqCst), 1);
        assert_eq!(self.cancellations.load(Ordering::SeqCst), 1);
    }
}

impl CommandBackend for ControlledBackend {
    fn execute<'a>(
        &'a self,
        id: &'a str,
        _: CommandSpec,
        phase: tokio::sync::watch::Sender<CommandStatus>,
    ) -> BoxFuture<'a, CommandResult> {
        Box::pin(async move {
            assert!(
                self.job.lock().replace(id.to_owned()).is_none(),
                "external execution is admitted once"
            );
            self.executions.fetch_add(1, Ordering::SeqCst);
            phase.send_replace(CommandStatus::CommandRunning);
            self.started.add_permits(1);
            self.finish.acquire().await.unwrap().forget();
            CommandResult {
                outcome: CommandOutcome::CommandCancelled,
                cleanup: if self.initial_cleanup_unknown {
                    CommandCleanup::CommandCleanupUnknown("controlled external cleanup".into())
                } else {
                    CommandCleanup::CommandClean
                },
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
            self.cancellations.fetch_add(1, Ordering::SeqCst);
            self.finish.add_permits(1);
            Ok(())
        })
    }

    fn output<'a>(
        &'a self,
        _: &'a str,
        _: usize,
    ) -> BoxFuture<'a, Result<CommandOutput, CommandError>> {
        Box::pin(async { Err(CommandError::CommandOutputPending) })
    }

    fn read<'a>(
        &'a self,
        _: &'a str,
        _: CommandStream,
        _: CommandPosition,
    ) -> BoxFuture<'a, Result<CommandPage, CommandError>> {
        Box::pin(async { Err(CommandError::CommandOutputPending) })
    }

    fn cleanup<'a>(&'a self, _: &'a str) -> BoxFuture<'a, CommandCleanup> {
        Box::pin(async move {
            self.cleanup_probes.fetch_add(1, Ordering::SeqCst);
            CommandCleanup::CommandClean
        })
    }
}

struct ScopeFixture {
    _root: tempfile::TempDir,
    forest: ResidentForest<frunk::HNil, tidepool_mcp::CapturedOutput>,
    deployments: mpsc::Receiver<LocalResidentDeployment>,
    backends: VecDeque<Arc<ControlledBackend>>,
    children: Vec<(String, LocalActorRef)>,
    retirements: Vec<ActorRef>,
}

impl ScopeFixture {
    fn new(case: u64, backends: Vec<Arc<ControlledBackend>>) -> Self {
        eval_harness::require_extract();
        let declarations = [
            tidepool_mcp::agent_tools_decl(),
            tidepool_mcp::agent_launch_decl(),
            tidepool_mcp::agent_control_decl(),
            tidepool_mcp::agent_inspection_decl(),
            tidepool_mcp::agent_session_decl(),
            tidepool_mcp::actor_decl(),
            tidepool_mcp::actor_kernel_decl(),
            tidepool_mcp::actor_local_decl(),
            tidepool_mcp::resource_scopes_decl(),
            tidepool_mcp::commands_decl(),
            tidepool_mcp::sleep_decl(),
        ];
        let effects = tidepool_mcp::ensure_effects_module(&declarations).expect("effect module");
        let include = crate::resident_workbench::request_tests::fixture_include_roots(&effects);
        let mut preamble = tidepool_mcp::build_notebook_preamble(&declarations, false);
        for import in [
            "Tidepool.Agent.Contract",
            "Control.Monad.Freer (Eff, Member)",
            "Data.Text (Text)",
            "qualified Tidepool.Actor as Mailbox",
            "qualified Tidepool.Scope as Scope",
            "qualified Tidepool.Command as Cmd",
            "qualified Tidepool.Actors.Spawn as Spawn",
            "qualified Tidepool.Actors.Internal.Agent as Agents",
            "Tidepool.Agent.Reply (Replies)",
            "qualified Tidepool.Agent.Reply as Reply",
            "Tidepool.Agent.Ref.Internal (AgentProtocol(..))",
            "qualified Tidepool.Agent.Ref.Internal as AgentRef",
            "qualified Tidepool.Effects.Core as Core",
        ] {
            preamble = insert_preamble_imports(&preamble, import);
        }
        let root = tempfile::tempdir().expect("session root");
        let session = tidepool_repr::SessionId(u64::from(std::process::id()) * 10_000 + case);
        let lib = SessionLib::open(session, root.path(), ModuleEnv::standalone_default())
            .expect("declaration plane")
            .with_validation_include(include.clone());
        let machine = ResidentSession::unbootstrapped(
            frunk::HNil,
            tidepool_mcp::CapturedOutput::new(),
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            Some(lib),
        );
        let (forest, deployments) = ResidentForest::new(
            ActorWorkbenchSource::new(preamble, include),
            session,
            machine,
            None,
            crate::Incarnation::FIRST,
        );
        Self {
            _root: root,
            forest,
            deployments,
            backends: backends.into(),
            children: Vec::new(),
            retirements: Vec::new(),
        }
    }

    async fn parent(&mut self) -> LocalActorRef {
        let parent = self
            .forest
            .new_workbench(
                "scope-runtime-parent".into(),
                crate::ActorCapabilities::default().with_effect_keys(vec![
                    crate::ActorEffectKey::Actor,
                    crate::ActorEffectKey::AgentLaunch,
                    crate::ActorEffectKey::AgentControl,
                    crate::ActorEffectKey::AgentInspection,
                    crate::ActorEffectKey::ResourceScopes,
                    crate::ActorEffectKey::Replies,
                    crate::ActorEffectKey::Commands,
                    crate::ActorEffectKey::Sleep,
                ]),
            )
            .await
            .expect("resident scope parent");
        let declarations = self
            .execute(
                &parent,
                include_str!("scope_runtime_declarations.hs"),
                None,
                None,
            )
            .await;
        assert_committed(declarations);
        let setup = self
            .execute(&parent, include_str!("scope_runtime_setup.hs"), None, None)
            .await;
        assert_committed(setup);
        parent
    }

    fn service(&mut self, event: LocalResidentDeployment) {
        match event {
            LocalResidentDeployment::PolicyInstalled(installation) => {
                installation
                    .spawn_admission
                    .as_ref()
                    .expect("idle spawn admission")
                    .acknowledge(installation.actor.identity())
                    .expect("attachment acknowledgement");
                self.children
                    .push((installation.label.clone(), installation.actor.clone()));
            }
            LocalResidentDeployment::CommandBackend(request) => {
                let backend = self
                    .backends
                    .pop_front()
                    .expect("exact authored command admission count");
                request.supply(Ok(backend));
            }
            LocalResidentDeployment::Retired { actor, .. } => self.retirements.push(actor),
            _ => {}
        }
    }

    async fn execute(
        &mut self,
        actor: &LocalActorRef,
        source: &str,
        control: Option<Arc<crate::WorkbenchExecutionControl>>,
        cancel_after_started: Option<Arc<ControlledBackend>>,
    ) -> crate::KernelWorkbenchReply {
        let (reply, receive) = tokio::sync::oneshot::channel();
        actor
            .address()
            .send_message(KernelMessage::Workbench {
                invocation: crate::ActorWorkbenchInvocation::unbound(
                    WorkbenchRequest::from_cell_input(source),
                ),
                control: control.clone(),
                reply: reply.into(),
            })
            .expect("production resident admission");
        let started = async {
            match cancel_after_started {
                Some(backend) => backend.started.acquire().await.unwrap().forget(),
                None => std::future::pending::<()>().await,
            }
        };
        tokio::pin!(started);
        tokio::pin!(receive);
        let mut cancel_sent = false;
        let result = tokio::time::timeout(Duration::from_secs(240), async {
            loop {
                tokio::select! {
                    reply = &mut receive => break reply.expect("resident settlement"),
                    event = self.deployments.recv() => self.service(event.expect("deployment receiver")),
                    () = &mut started, if !cancel_sent => {
                        assert!(control.as_ref().expect("cancel control").request_cancellation());
                        cancel_sent = true;
                    }
                }
            }
        }).await.expect("bounded real resident scope execution");
        while let Ok(event) = self.deployments.try_recv() {
            self.service(event);
        }
        result
    }

    fn child(&self, label: &str) -> LocalActorRef {
        self.children
            .iter()
            .find(|(observed, _)| observed == label)
            .expect("exact installed scope child")
            .1
            .clone()
    }

    fn assert_closed_owner_cleanup(&self, actor: ActorRef) {
        let roots = self
            .forest
            .environment
            .actors
            .lock()
            .get(&actor)
            .unwrap()
            .workbench_executions
            .lock()
            .invocation_work();
        let scopes = roots
            .iter()
            .flat_map(|root| root.scopes())
            .collect::<Vec<_>>();
        assert!(!scopes.is_empty(), "actual runtime admitted lexical owners");
        for scope in scopes {
            assert!(scope.is_closed());
            assert_eq!(
                scope
                    .cleanup_observation()
                    .expect("retained cleanup receipt")
                    .uncertainty(),
                None
            );
        }
    }

    fn assert_request_cancelled_without_target_retirement(&self, owner: ActorRef) {
        let status = self.forest.environment.requests.status_for(owner);
        assert!(status.pending_responses.is_empty());
        let request = status
            .unavailable_responses
            .iter()
            .find_map(|(id, _, failure)| {
                matches!(failure, ResponseFailure::Cancelled).then_some(*id)
            })
            .expect("scope request cancelled by its actual owner");
        assert_eq!(
            self.forest
                .environment
                .requests
                .request_cleanup_state(owner, request),
            Ok(RequestCleanupState::TargetClosed)
        );
        let target = self
            .forest
            .environment
            .requests
            .target_for(request)
            .unwrap();
        assert!(
            self.forest
                .directory
                .resolve(target)
                .unwrap()
                .terminal()
                .get()
                .is_none(),
            "request cleanup does not retire its borrowed target"
        );
        assert!(matches!(
            self.forest
                .environment
                .requests
                .request_cleanup_owner(owner, request),
            Ok(ResourceCleanupOwner::Scope(_))
        ));
    }

    fn assert_scope_membership(
        &self,
        owner: ActorRef,
        child: ActorRef,
        backend: &ControlledBackend,
    ) {
        let roots = self
            .forest
            .environment
            .actors
            .lock()
            .get(&owner)
            .unwrap()
            .workbench_executions
            .lock()
            .invocation_work();
        let scope = roots
            .iter()
            .flat_map(|root| root.scopes())
            .find(|scope| scope.owns_worker(child))
            .expect("actual child cleanup belongs to its lexical scope");
        let token = scope.scope_token().unwrap();
        let job = backend
            .job
            .lock()
            .clone()
            .expect("actual native command identity");
        assert_eq!(
            self.forest
                .environment
                .commands
                .cleanup_owner(owner, &job)
                .unwrap(),
            ResourceCleanupOwner::Scope(token)
        );
        let request = self
            .forest
            .environment
            .requests
            .status_for(owner)
            .unavailable_responses
            .into_iter()
            .find_map(|(request, _, failure)| {
                matches!(failure, ResponseFailure::Cancelled).then_some(request)
            })
            .expect("actual cancelled scoped request");
        assert_eq!(
            self.forest
                .environment
                .requests
                .request_cleanup_owner(owner, request)
                .unwrap(),
            ResourceCleanupOwner::Scope(token)
        );
    }

    async fn finish(self) {
        assert!(
            self.backends.is_empty(),
            "no expected admission was skipped"
        );
        assert!(self
            .forest
            .shutdown()
            .await
            .iter()
            .all(crate::ForestRootShutdown::is_confirmed));
    }
}

fn assert_committed(reply: crate::KernelWorkbenchReply) {
    let response = reply.expect("public Haskell scope contract");
    assert_eq!(
        response.status,
        WorkbenchRunStatus::Committed,
        "{response:?}"
    );
    assert_eq!(
        response.items.last().map(|item| item.output.trim()),
        Some("True"),
        "final Haskell fixture result must confirm its assertions: {response:?}"
    );
}

#[tokio::test]
async fn public_scope_owns_resources_preserves_retention_and_retries_cleanup_without_replay() {
    let normal = ControlledBackend::new(false);
    let uncertain = ControlledBackend::new(true);
    let mut fixture = ScopeFixture::new(731, vec![normal.clone(), uncertain.clone()]);
    let parent = fixture.parent().await;
    let result = fixture
        .execute(&parent, include_str!("scope_runtime_normal.hs"), None, None)
        .await;
    assert_committed(result);
    assert_eq!(
        fixture.children.len(),
        2,
        "closed-token refusal cannot launch a child"
    );
    let owned = fixture.child("scope-owned-child");
    assert_eq!(
        owned.terminal().get().unwrap().kind,
        ActorExitKind::Cancelled
    );
    assert_eq!(
        fixture
            .retirements
            .iter()
            .filter(|actor| **actor == owned.identity())
            .count(),
        1
    );
    let survivor = fixture.child("scope-retained-child");
    assert!(survivor.terminal().get().is_none());
    fixture.assert_request_cancelled_without_target_retirement(parent.identity());
    normal.assert_executed_once();
    fixture.assert_scope_membership(parent.identity(), owned.identity(), &normal);

    // Use the installed child tool after the body realm and owner have closed.
    // No hosted model inference participates in this retained closure call.
    let answer = tokio::time::timeout(
        Duration::from_secs(240),
        crate::ResidentInteractivePolicy::local(survivor).dispatch_boxed(
            exomonad_tool::ToolInvocation {
                context: None,
                name: "ping".into(),
                arguments: exomonad_tool::ToolArguments::Structured(
                    serde_json::json!({"sentinel": 0}),
                ),
            },
        ),
    )
    .await
    .expect("retained child call is bounded")
    .expect("retained child tool remains callable")
    .into_json()
    .unwrap();
    assert!(answer.to_string().contains("73"), "{answer:?}");

    let result = fixture
        .execute(
            &parent,
            include_str!("scope_runtime_uncertain.hs"),
            None,
            None,
        )
        .await;
    assert_committed(result);
    uncertain.assert_executed_once();
    assert!(
        uncertain.cleanup_probes.load(Ordering::SeqCst) >= 1,
        "the original owner retried external cleanup"
    );
    fixture.assert_closed_owner_cleanup(parent.identity());
    fixture.finish().await;
}

#[tokio::test]
async fn public_scope_body_failure_closes_real_resources_with_independent_cleanup_result() {
    let backend = ControlledBackend::new(false);
    let mut fixture = ScopeFixture::new(732, vec![backend.clone()]);
    let parent = fixture.parent().await;
    let result = fixture
        .execute(
            &parent,
            include_str!("scope_runtime_failure.hs"),
            None,
            None,
        )
        .await;
    assert_committed(result);
    assert_eq!(fixture.children.len(), 1);
    assert_eq!(
        fixture
            .child("scope-failed-child")
            .terminal()
            .get()
            .unwrap()
            .kind,
        ActorExitKind::Cancelled
    );
    fixture.assert_request_cancelled_without_target_retirement(parent.identity());
    fixture.assert_closed_owner_cleanup(parent.identity());
    backend.assert_executed_once();
    fixture.assert_scope_membership(
        parent.identity(),
        fixture.child("scope-failed-child").identity(),
        &backend,
    );
    fixture.finish().await;
}

#[tokio::test]
async fn public_scope_outer_cancellation_retains_real_cleanup_without_resuming_body() {
    let backend = ControlledBackend::new(false);
    let mut fixture = ScopeFixture::new(733, vec![backend.clone()]);
    let parent = fixture.parent().await;
    let control = crate::WorkbenchExecutionControl::untracked();
    let result = fixture
        .execute(
            &parent,
            include_str!("scope_runtime_cancel.hs"),
            Some(control.clone()),
            Some(backend.clone()),
        )
        .await;
    assert!(
        result.as_ref().map_or(true, |response| response.status
            != WorkbenchRunStatus::Committed),
        "cancelled outer invocation must not commit its body: {result:?}"
    );
    assert!(matches!(
        control.cancellation_outcome(control.execution_id(parent.identity()), result),
        crate::WorkbenchCancellationOutcome::Cancelled { .. }
    ));
    assert_eq!(fixture.children.len(), 1);
    assert_eq!(
        fixture
            .child("scope-cancelled-child")
            .terminal()
            .get()
            .unwrap()
            .kind,
        ActorExitKind::Cancelled
    );
    fixture.assert_request_cancelled_without_target_retirement(parent.identity());
    fixture.assert_closed_owner_cleanup(parent.identity());
    backend.assert_executed_once();
    fixture.assert_scope_membership(
        parent.identity(),
        fixture.child("scope-cancelled-child").identity(),
        &backend,
    );
    fixture.finish().await;
}
