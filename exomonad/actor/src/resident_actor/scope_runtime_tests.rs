//! Public lexical scopes through the production resident workbench. The only
//! controlled services are the external command backend and SameDir workspace
//! attachment, backed by real worktree registration and binding leases. Idle
//! tool installation is acknowledged without a provider round.

use super::*;
use crate::command_jobs::{CommandBackend, CommandControl};
use crate::fork_workspace::{
    PreparedWorkspaceAttachment, WorkspaceAdmission, WorkspaceAdmissionError,
    WorkspaceAdmissionFuture, WorkspaceCustody, WorkspaceSelection,
};
use crate::request::{RequestCleanupState, ResourceCleanupOwner, ResponseFailure};
use exomonad_worktree::{
    ActiveBinding, BindingTable, WorkspaceAccess, WorktreeManager, WorktreeRegistry,
};
use futures_util::future::BoxFuture;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tidepool_bridge_effects::{
    CommandCleanup, CommandError, CommandOutcome, CommandOutput, CommandPage, CommandPosition,
    CommandResult, CommandSpec, CommandStatus, CommandStream, WtWorktreeHandle,
};
use tidepool_handlers::handlers::worktree::{
    handle_to_wire, ActorWorktreeAuthority, ActorWorktreeHandler, WorktreeHandler,
};
use tidepool_runtime::session::{insert_preamble_imports, ModuleEnv, SessionLib};
use tidepool_testing::eval_harness;

#[derive(Clone)]
struct ScopeWorkspace {
    runtime: String,
    handle: WtWorktreeHandle,
    bindings: Arc<Mutex<BindingTable>>,
}

impl ScopeWorkspace {
    fn principal(&self, actor: ActorRef) -> exomonad_worktree::AgentRef {
        exomonad_worktree::AgentRef::exact_actor(&self.runtime, actor.id.0, actor.incarnation.0)
    }

    fn bind(&self, actor: ActorRef) -> Result<ActiveBinding, WorkspaceAdmissionError> {
        self.bindings
            .lock()
            .bind(
                &exomonad_worktree::WorktreeId::from_raw(&self.handle.handle_receipt.tree_id.raw),
                &self.principal(actor),
                WorkspaceAccess::ReadWrite,
                exomonad_worktree::storage::now_ms(),
            )
            .map_err(|error| WorkspaceAdmissionError {
                detail: error.to_string(),
            })
    }
}

struct ScopeWorkspaceCustody {
    bindings: Arc<Mutex<BindingTable>>,
    binding: Mutex<Option<ActiveBinding>>,
}

impl WorkspaceCustody for ScopeWorkspaceCustody {
    fn actor_stopped(&self, terminal: &crate::ActorTerminal) {
        if let Some(binding) = self.binding.lock().take() {
            let mut table = self.bindings.lock();
            let result = if terminal.kind == ActorExitKind::Completed {
                binding.complete(&mut table)
            } else {
                binding.release(&mut table)
            };
            result.expect("real scope workspace binding settles");
        }
    }

    fn process_may_exist(&self) {
        panic!("scope fixture never submits an external actor process");
    }
}

impl WorkspaceAdmission for ScopeWorkspace {
    fn prepare(
        &self,
        owner: ActorRef,
        selection: WorkspaceSelection,
        access: Option<WorkspaceAccess>,
    ) -> WorkspaceAdmissionFuture<'_> {
        Box::pin(async move {
            if !matches!(selection, WorkspaceSelection::SameDirectory)
                || access.is_some_and(|access| access != WorkspaceAccess::ReadWrite)
            {
                return Err(WorkspaceAdmissionError {
                    detail: "scope fixture supports only writable SameDir attachment".into(),
                });
            }
            let tree =
                exomonad_worktree::WorktreeId::from_raw(&self.handle.handle_receipt.tree_id.raw);
            if self
                .bindings
                .lock()
                .membership(&tree, &self.principal(owner))
                .is_none()
            {
                return Err(WorkspaceAdmissionError {
                    detail: "scope fixture caller has no active workspace binding".into(),
                });
            }
            let workspace = self.clone();
            Ok(PreparedWorkspaceAttachment::new(
                self.handle.clone(),
                move |actor| {
                    let binding = workspace.bind(actor)?;
                    Ok(Arc::new(ScopeWorkspaceCustody {
                        bindings: workspace.bindings,
                        binding: Mutex::new(Some(binding)),
                    }))
                },
            ))
        })
    }
}

struct ControlledBackend {
    executions: AtomicUsize,
    cancellations: AtomicUsize,
    cleanup_probes: AtomicUsize,
    started: tokio::sync::Semaphore,
    finish: tokio::sync::Semaphore,
    initial_cleanup_unknown: bool,
    job: Mutex<Option<String>>,
    request: Mutex<Option<Arc<RequestGates>>>,
}

struct RequestGates {
    activations: AtomicUsize,
    activated: tokio::sync::Semaphore,
    closed: tokio::sync::Semaphore,
}

impl RequestGates {
    fn new() -> Self {
        Self {
            activations: AtomicUsize::new(0),
            activated: tokio::sync::Semaphore::new(0),
            closed: tokio::sync::Semaphore::new(0),
        }
    }
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
            request: Mutex::new(None),
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
            let request = self.request.lock().clone();
            if let Some(request) = request {
                request.activated.acquire().await.unwrap().forget();
            }
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
            let request = self.request.lock().clone();
            if let Some(request) = request {
                request.closed.acquire().await.unwrap().forget();
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
    _repository: exomonad_worktree::testing::TestRepo,
    workspace: Arc<ScopeWorkspace>,
    parent_binding: Option<ActiveBinding>,
    forest: ResidentForest<
        frunk::HCons<ActorWorktreeHandler, frunk::HNil>,
        tidepool_mcp::CapturedOutput,
    >,
    deployments: mpsc::Receiver<LocalResidentDeployment>,
    backends: VecDeque<Arc<ControlledBackend>>,
    children: Vec<(String, LocalActorRef, Arc<dyn crate::ResidentToolEndpoint>)>,
    retirements: Vec<ActorRef>,
    request_target: Option<LocalActorRef>,
    request_policy: Option<Arc<dyn crate::ResidentToolEndpoint>>,
    request_gates: Arc<RequestGates>,
}

impl ScopeFixture {
    async fn new(case: u64, backends: Vec<Arc<ControlledBackend>>) -> Self {
        eval_harness::require_extract();
        let declarations = [
            tidepool_mcp::console_decl(),
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
            tidepool_mcp::worktree_decl(),
        ];
        let effects = tidepool_mcp::ensure_effects_module(&declarations).expect("effect module");
        let include = crate::resident_workbench::request_tests::fixture_include_roots(&effects);
        let mut preamble = tidepool_mcp::build_notebook_preamble(&declarations, false);
        for import in [
            "Tidepool.Agent.Contract",
            "Control.Monad.Freer (Eff, Member)",
            "Data.Text (Text)",
            "qualified Data.Text as Text",
            "qualified Tidepool.Scope as Scope",
            "qualified Tidepool.Command as Cmd",
            "qualified Tidepool.Actors.Spawn as Spawn",
            "qualified Tidepool.Actors.Internal.Agent as Agents",
            "Tidepool.Agent.Reply (Replies)",
            "qualified Tidepool.Agent.Reply as Reply",
            "qualified Tidepool.Agent.Ref.Internal as AgentRef",
            "qualified Tidepool.Effects.Core as Core",
        ] {
            preamble = insert_preamble_imports(&preamble, import);
        }
        let root = tempfile::tempdir().expect("session root");
        let repository = exomonad_worktree::testing::TestRepo::init_in(root.path())
            .expect("real workspace repository");
        repository
            .writer()
            .commit_empty("scope fixture workspace")
            .expect("workspace source commit");
        let anchor = tidepool_atomic_write::DirectoryAnchor::open_existing(root.path())
            .expect("workspace storage anchor");
        let manager = WorktreeManager::new(
            repository.git().clone(),
            WorktreeRegistry::open(&anchor, "workspace-registry").expect("workspace registry"),
            root.path().join("managed-workspaces"),
            repository.path(),
        );
        let source = manager
            .register_source_checkout()
            .expect("registered scope workspace");
        let workspace = Arc::new(ScopeWorkspace {
            runtime: format!("scope-runtime-{}-{case}", std::process::id()),
            bindings: Arc::new(Mutex::new(
                BindingTable::open(&anchor, "workspace-bindings").expect("workspace binding owner"),
            )),
            handle: handle_to_wire(&source),
        });
        let worktree = ActorWorktreeHandler::new(
            WorktreeHandler::from_manager(manager),
            ActorWorktreeAuthority::new(workspace.runtime.as_str(), workspace.bindings.clone()),
        );
        let session = tidepool_repr::SessionId(u64::from(std::process::id()) * 10_000 + case);
        let lib = SessionLib::open(session, root.path(), ModuleEnv::standalone_default())
            .expect("declaration plane")
            .with_validation_include(include.clone());
        let machine = ResidentSession::unbootstrapped(
            frunk::hlist![worktree],
            tidepool_mcp::CapturedOutput::new(),
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            Some(lib),
        );
        let (forest, deployments) = ResidentForest::new(
            ActorWorkbenchSource::new(preamble, include),
            session,
            machine,
            Some(workspace.clone()),
            crate::Incarnation::FIRST,
        )
        .await
        .expect("actual run-control owner");
        let request_gates = Arc::new(RequestGates::new());
        // Pair the first command with the borrowed request: activation precedes
        // execution, and native acknowledgement precedes terminal cleanup.
        // The later command tests only external cleanup uncertainty and retry.
        if let Some(backend) = backends.first() {
            *backend.request.lock() = Some(request_gates.clone());
        }
        Self {
            _root: root,
            _repository: repository,
            workspace,
            parent_binding: None,
            forest,
            deployments,
            backends: backends.into(),
            children: Vec::new(),
            retirements: Vec::new(),
            request_target: None,
            request_policy: None,
            request_gates,
        }
    }

    async fn parent(&mut self) -> LocalActorRef {
        let parent = self
            .forest
            .new_workbench(
                "scope-runtime-parent".into(),
                crate::ActorCapabilities::default().with_effect_keys(vec![
                    crate::ActorEffectKey::Console,
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
        self.parent_binding = Some(
            self.workspace
                .bind(parent.identity())
                .expect("parent workspace binding"),
        );
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

    fn service(
        &mut self,
        event: LocalResidentDeployment,
    ) -> Option<(
        crate::RequestId,
        crate::resident_tools::ResidentToolDispatchFuture,
    )> {
        match event {
            LocalResidentDeployment::PolicyInstalled(installation) => {
                installation
                    .spawn_admission
                    .as_ref()
                    .expect("idle spawn admission")
                    .acknowledge(installation.actor.identity())
                    .expect("attachment acknowledgement");
                if installation.label == "scope-request-target" {
                    self.request_policy = Some(installation.policy.clone());
                    assert!(self
                        .request_target
                        .replace(installation.actor.clone())
                        .is_none());
                } else {
                    self.children.push((
                        installation.label.clone(),
                        installation.actor.clone(),
                        installation.policy.clone(),
                    ));
                }
            }
            LocalResidentDeployment::CommandBackend(request) => {
                let backend = self
                    .backends
                    .pop_front()
                    .expect("exact authored command admission count");
                request.supply(Ok(backend));
            }
            LocalResidentDeployment::Retired { actor, terminal } => {
                assert!(
                    !self
                        .request_target
                        .as_ref()
                        .is_some_and(|target| target.identity() == actor),
                    "borrowed request target retired before fixture shutdown: {terminal:?}"
                );
                self.retirements.push(actor);
            }
            LocalResidentDeployment::SessionReady { activation } => {
                assert_eq!(
                    activation.id.actor(),
                    self.request_target.as_ref().unwrap().identity()
                );
                self.request_gates
                    .activations
                    .fetch_add(1, Ordering::SeqCst);
                self.request_gates.activated.add_permits(1);
            }
            LocalResidentDeployment::RequestCancellation { notification } => {
                let target = self
                    .request_target
                    .as_ref()
                    .expect("cooperative request target");
                assert_eq!(notification.target, target.identity());
                return Some((
                    notification.request,
                    self.request_policy.as_ref().unwrap().dispatch_boxed(
                        exomonad_tool::ToolInvocation {
                            context: None,
                            name: "acknowledge".into(),
                            arguments: exomonad_tool::ToolArguments::Structured(
                                serde_json::json!({"sentinel": 0}),
                            ),
                        },
                    ),
                ));
            }
            _ => {}
        }
        None
    }

    async fn execute(
        &mut self,
        actor: &LocalActorRef,
        source: &str,
        control: Option<Arc<crate::WorkbenchExecutionControl>>,
        cancel_after_started: Option<Arc<ControlledBackend>>,
    ) -> crate::KernelWorkbenchReply {
        self.execute_until_cleanup(actor, source, control, cancel_after_started, 0)
            .await
    }

    async fn execute_until_cleanup(
        &mut self,
        actor: &LocalActorRef,
        source: &str,
        control: Option<Arc<crate::WorkbenchExecutionControl>>,
        cancel_after_started: Option<Arc<ControlledBackend>>,
        cancelled_targets: usize,
    ) -> crate::KernelWorkbenchReply {
        let receive = send_workbench(actor, source, control.clone()).await;
        let started = async {
            match cancel_after_started {
                Some(backend) => backend.started.acquire().await.unwrap().forget(),
                None => std::future::pending::<()>().await,
            }
        };
        tokio::pin!(started);
        tokio::pin!(receive);
        let mut cancel_sent = false;
        let mut target_acknowledgement = None;
        let mut settled = None;
        let mut observation = tokio::time::interval(Duration::from_millis(1));
        let result = tokio::time::timeout(Duration::from_secs(240), async {
            loop {
                if settled.is_some() && target_acknowledgement.is_none() && self.cancelled_targets_closed(actor.identity()) >= cancelled_targets {
                    break settled.take().unwrap();
                }
                tokio::select! {
                    reply = &mut receive, if settled.is_none() => {
                        let reply = reply.expect("resident settlement");
                        if !reply.as_ref().is_ok_and(|response| response.status == tidepool_runtime::session::WorkbenchRunStatus::Committed) {
                            break reply;
                        }
                        settled = Some(reply);
                    },
                    _ = observation.tick(), if settled.is_some() => {},
                    event = self.deployments.recv() => {
                        if let Some(receive) = self.service(event.expect("deployment receiver")) {
                            assert!(target_acknowledgement.replace(receive).is_none(), "one target cancellation acknowledgement");
                        }
                    },
                    acknowledgement = async {
                        match target_acknowledgement.as_mut() {
                            Some((request, receive)) => (*request, receive.await),
                            None => std::future::pending().await,
                        }
                    } => {
                        let (request, response) = acknowledgement;
                        let crate::ResidentToolResponse::Workbench(response) = response.expect("target installed acknowledgement") else {
                            panic!("target acknowledgement must return native workbench receipts");
                        };
                        assert_eq!(response.items.last().and_then(|item| item.terminal_transfer), Some(tidepool_runtime::session::WorkbenchTerminalTransfer::CancellationAcknowledged), "{response:?}");
                        while self.forest.environment.requests.request_cleanup_state(actor.identity(), request) != Ok(RequestCleanupState::TargetClosed) {
                            tokio::task::yield_now().await;
                        }
                        target_acknowledgement = None;
                        self.request_gates.closed.add_permits(1);
                    },
                    () = &mut started, if !cancel_sent => {
                        assert!(control.as_ref().expect("cancel control").request_cancellation());
                        cancel_sent = true;
                    }
                }
            }
        }).await.expect("bounded real resident scope execution");
        while let Ok(event) = self.deployments.try_recv() {
            assert!(
                self.service(event).is_none(),
                "native target acknowledgement must settle before caller cleanup"
            );
        }
        result
    }

    fn cancelled_targets_closed(&self, owner: ActorRef) -> usize {
        self.forest
            .environment
            .requests
            .status_for(owner)
            .unavailable_responses
            .into_iter()
            .filter(|(request, _, failure)| {
                matches!(failure, ResponseFailure::Cancelled)
                    && self
                        .forest
                        .environment
                        .requests
                        .request_cleanup_state(owner, *request)
                        == Ok(RequestCleanupState::TargetClosed)
            })
            .count()
    }

    async fn wait_next_activation(&mut self) {
        tokio::time::timeout(Duration::from_secs(240), async {
            loop {
                if let Ok(permit) = self.request_gates.activated.try_acquire() {
                    permit.forget();
                    break;
                }
                let event = self
                    .deployments
                    .recv()
                    .await
                    .expect("native request activation");
                assert!(
                    self.service(event).is_none(),
                    "request activation is not cancellation"
                );
            }
        })
        .await
        .expect("native request activation is bounded");
    }

    fn child(&self, label: &str) -> LocalActorRef {
        self.children
            .iter()
            .find(|(observed, _, _)| observed == label)
            .expect("exact installed scope child")
            .1
            .clone()
    }

    fn child_policy(&self, label: &str) -> Arc<dyn crate::ResidentToolEndpoint> {
        self.children
            .iter()
            .find(|(observed, _, _)| observed == label)
            .expect("exact installed scope child policy")
            .2
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

    async fn assert_scoped_request_cancelled_without_target_retirement(&self, owner: ActorRef) {
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
                .await
                .expect("directory actor observation")
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

    async fn assert_target_reusable(&mut self, owner: &LocalActorRef) {
        assert_committed(
            self.execute(
                owner,
                include_str!("scope_runtime_request_again.hs"),
                None,
                None,
            )
            .await,
        );
        self.wait_next_activation().await;
        let requests = &self.forest.environment.requests;
        let target = self.request_target.as_ref().unwrap().identity();
        let active = requests.open_without_reply(target).unwrap();
        assert_eq!(
            requests.begin_reply(owner.identity(), active),
            Err(crate::ReplyError::Unauthorized),
            "the requester cannot settle its target's native request"
        );
        assert_eq!(requests.open_without_reply(target), Some(active));
        let result = tokio::time::timeout(
            Duration::from_secs(240),
            self.request_policy
                .as_ref()
                .unwrap()
                .dispatch_boxed(exomonad_tool::ToolInvocation {
                    context: None,
                    name: "respond".into(),
                    arguments: exomonad_tool::ToolArguments::Structured(
                        serde_json::json!({"sentinel": 91}),
                    ),
                }),
        )
        .await
        .expect("second native response is bounded")
        .expect("second installed response");
        let crate::ResidentToolResponse::Workbench(response) = result else {
            panic!("native response receipts")
        };
        assert_eq!(
            response
                .items
                .last()
                .and_then(|item| item.terminal_transfer),
            Some(tidepool_runtime::session::WorkbenchTerminalTransfer::ReplyAccepted)
        );
        assert_committed(
            self.execute(
                owner,
                include_str!("scope_runtime_observe_again.hs"),
                None,
                None,
            )
            .await,
        );
        assert!(self
            .request_target
            .as_ref()
            .unwrap()
            .terminal()
            .get()
            .is_none());
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
        self.parent_binding
            .expect("parent workspace lease")
            .release(&mut self.workspace.bindings.lock())
            .expect("parent workspace release");
        assert!(
            self.workspace
                .bindings
                .lock()
                .participants(&exomonad_worktree::WorktreeId::from_raw(
                    &self.workspace.handle.handle_receipt.tree_id.raw
                ))
                .expect("workspace participants")
                .next()
                .is_none(),
            "all fixture workspace bindings settled"
        );
    }
}

async fn send_workbench(
    actor: &LocalActorRef,
    source: &str,
    control: Option<Arc<crate::WorkbenchExecutionControl>>,
) -> tokio::sync::oneshot::Receiver<crate::KernelWorkbenchReply> {
    let (reply, receive) = tokio::sync::oneshot::channel();
    actor
        .submit_workbench(
            crate::ActorWorkbenchInvocation::unbound(WorkbenchRequest::from_cell_input(source)),
            control,
            reply.into(),
        )
        .await
        .expect("production resident admission");
    receive
}

// Notebook expressions bind observations. These fixtures explicitly render
// their Boolean assertion through Console and discard the final statement.
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
    let mut fixture = ScopeFixture::new(731, vec![normal.clone(), uncertain.clone()]).await;
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
    fixture
        .assert_scoped_request_cancelled_without_target_retirement(parent.identity())
        .await;
    normal.assert_executed_once();
    fixture.assert_scope_membership(parent.identity(), owned.identity(), &normal);

    // Use the installed child tool after the body realm and owner have closed.
    // No hosted model inference participates in this retained closure call.
    let answer = tokio::time::timeout(
        Duration::from_secs(240),
        fixture.child_policy("scope-retained-child").dispatch_boxed(
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
    fixture.assert_target_reusable(&parent).await;
    fixture.finish().await;
}

#[tokio::test]
async fn public_scope_body_failure_closes_real_resources_with_independent_cleanup_result() {
    let backend = ControlledBackend::new(false);
    let mut fixture = ScopeFixture::new(732, vec![backend.clone()]).await;
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
    fixture
        .assert_scoped_request_cancelled_without_target_retirement(parent.identity())
        .await;
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
    let mut fixture = ScopeFixture::new(733, vec![backend.clone()]).await;
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
    let cancellation =
        control.cancellation_outcome(control.execution_id(parent.identity()), result);
    assert!(
        matches!(
            cancellation,
            crate::WorkbenchCancellationOutcome::Cancelled { .. }
        ),
        "cancellation must reach the controlled command after child installation: {cancellation:?}"
    );
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
    fixture
        .assert_scoped_request_cancelled_without_target_retirement(parent.identity())
        .await;
    fixture.assert_closed_owner_cleanup(parent.identity());
    backend.assert_executed_once();
    fixture.assert_scope_membership(
        parent.identity(),
        fixture.child("scope-cancelled-child").identity(),
        &backend,
    );
    fixture.finish().await;
}

#[tokio::test]
async fn native_queued_cancellation_preserves_receiver_and_accepts_later_request() {
    if std::env::var_os("RUST_LOG").is_some() {
        let _ = tracing_subscriber::fmt()
            .json()
            .with_ansi(false)
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .try_init();
    }
    let mut fixture = ScopeFixture::new(734, Vec::new()).await;
    let parent = fixture.parent().await;
    assert_committed(
        fixture
            .execute(
                &parent,
                include_str!("scope_runtime_request_again.hs"),
                None,
                None,
            )
            .await,
    );
    fixture.wait_next_activation().await;
    assert_eq!(fixture.request_gates.activations.load(Ordering::SeqCst), 1);

    // The active first request excludes receiver delivery of the second.
    // Cancelling it before the first proves the native unpresented branch.
    assert_committed(
        fixture
            .execute_until_cleanup(
                &parent,
                include_str!("scope_runtime_cancel_queued.hs"),
                None,
                None,
                2,
            )
            .await,
    );
    assert_eq!(fixture.cancelled_targets_closed(parent.identity()), 2);
    assert_eq!(
        fixture.request_gates.activations.load(Ordering::SeqCst),
        1,
        "cancelled queued request must never activate"
    );
    let status = fixture
        .forest
        .environment
        .requests
        .status_for(parent.identity());
    assert!(status.pending_responses.is_empty());
    assert_eq!(status.unavailable_responses.len(), 2);
    let target = fixture.request_target.as_ref().unwrap().identity();
    for (request, _, failure) in status.unavailable_responses {
        assert_eq!(failure, ResponseFailure::Cancelled);
        assert_eq!(
            fixture.forest.environment.requests.target_for(request),
            Some(target)
        );
        assert_eq!(
            fixture
                .forest
                .environment
                .requests
                .request_cleanup_owner(parent.identity(), request),
            Ok(ResourceCleanupOwner::Actor)
        );
    }
    assert!(
        fixture
            .request_target
            .as_ref()
            .unwrap()
            .terminal()
            .get()
            .is_none(),
        "native cancellation leaves the borrowed target live"
    );
    fixture.assert_target_reusable(&parent).await;
    assert_eq!(fixture.request_gates.activations.load(Ordering::SeqCst), 2);
    fixture.finish().await;
}
