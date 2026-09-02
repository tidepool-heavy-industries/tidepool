//! Resident Haskell behavior owned directly by one local actor.
//!
//! Ractor serializes logical turns and owns the mailbox. The shared resident
//! machine registry owns only short-lived machine checkout. This module is
//! the single driver between those boundaries; it does not recreate registry
//! turn leases, parked-obligation maps, or a host-side scheduler.

use std::sync::Arc;

use parking_lot::Mutex;
use tidepool_codegen::suspension::RealmId;
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_model::{DynModelProvider, StreamSink};
use tidepool_runtime::session::{
    OutputSink, ParsedBlock, ResidentOutcome, ResidentSession, RootCustody, WorkbenchItemReceipt,
    WorkbenchItemStatus, WorkbenchRequest, WorkbenchResponse, WorkbenchRunStatus,
};
use tokio::sync::mpsc;

use crate::mailbox::{InstalledReceiver, ResidentOutbound};
use crate::resident_workbench::{
    ResidentActorBoundary, ResidentActorStartupStep, ResidentKernelBoundary,
    ResidentWorkbenchFragment, ResidentWorkbenchStep,
};
use crate::{
    ActorAgentSession, ActorDescriptor, ActorExitKind, ActorMachineRegistry, ActorRef,
    ActorSessionContext, ActorTerminal, ActorWorkbenchSource, ChildExitNotice,
    ExternalApplicationFailure, ExternalFailureDisposition, KernelBehavior, KernelBehaviorError,
    KernelCallFailure, KernelContext, KernelInvocationFailure, KernelMessage, KernelStep,
    LocalActorRef, MailboxValue, ResidentActorRunner, ResidentActorWorkbenchError,
    ResidentCompletionExecutor, ResidentMcpEndpoint,
};

/// A compiled root at the point where ownership moves into its local actor.
pub struct ResidentActorRoot<H, O> {
    descriptor: ActorDescriptor,
    machine: ResidentSession<H, O>,
    outcome: ResidentOutcome,
}

impl<H, O> ResidentActorRoot<H, O> {
    #[must_use]
    pub fn new(
        descriptor: ActorDescriptor,
        machine: ResidentSession<H, O>,
        outcome: ResidentOutcome,
    ) -> Self {
        Self {
            descriptor,
            machine,
            outcome,
        }
    }

    fn into_parts(self) -> (ActorDescriptor, ResidentSession<H, O>, ResidentOutcome) {
        (self.descriptor, self.machine, self.outcome)
    }
}

#[derive(Clone)]
pub struct LocalResidentInstallation {
    pub actor: LocalActorRef,
    pub label: String,
    pub policy: Arc<dyn ResidentMcpEndpoint>,
    pub initial_user_message: Option<String>,
    pub launch_worktrees: Vec<String>,
}

#[derive(Clone)]
pub enum LocalResidentDeployment {
    PolicyInstalled(LocalResidentInstallation),
    ChildExited {
        notice: ChildExitNotice,
        worker_wake: Option<crate::WorkerWake>,
    },
    Retired {
        actor: ActorRef,
        terminal: ActorTerminal,
    },
}

struct ResidentEnvironment<H, O> {
    runner: ResidentActorRunner<H, O>,
    completions: ResidentCompletionExecutor<H, O>,
    provider: Arc<dyn DynModelProvider>,
    sink: Option<StreamSink>,
    deployments: mpsc::UnboundedSender<LocalResidentDeployment>,
    workers: Arc<crate::worker_runtime::WorkerRuntime>,
    pending_worker_installations:
        Arc<Mutex<std::collections::HashMap<ActorRef, LocalResidentInstallation>>>,
    retired: Arc<Mutex<std::collections::HashSet<ActorRef>>>,
}

impl<H, O> Clone for ResidentEnvironment<H, O> {
    fn clone(&self) -> Self {
        Self {
            runner: self.runner.clone(),
            completions: self.completions.clone(),
            provider: Arc::clone(&self.provider),
            sink: self.sink.clone(),
            deployments: self.deployments.clone(),
            workers: Arc::clone(&self.workers),
            pending_worker_installations: Arc::clone(&self.pending_worker_installations),
            retired: Arc::clone(&self.retired),
        }
    }
}

enum ResidentBoot {
    Prepared(Box<ResidentOutcome>),
    Entry(RootCustody),
}

enum ResidentStanding {
    Boot,
    Receiving(InstalledReceiver),
    Mcp(crate::resident_mcp::ResidentMcpAwait),
    Interactive(crate::interactive_session::ResidentInteractiveAwait),
    Terminal,
}

/// All actor-local resident state. No field mirrors runnable/parked lifecycle;
/// `standing` is the actual Haskell continuation currently owned by the actor.
pub struct ResidentKernelBehavior<H, O> {
    descriptor: ActorDescriptor,
    environment: ResidentEnvironment<H, O>,
    boot: Option<ResidentBoot>,
    standing: ResidentStanding,
    session: Option<ActorAgentSession>,
    shutdown_hook: Option<RootCustody>,
    launch_worktrees: Vec<String>,
    policy_installed: bool,
    activation_wakes: Option<Vec<crate::WorkerWake>>,
    owns_worker_runtime: bool,
}

impl<H, O> ResidentKernelBehavior<H, O> {
    fn prepared(
        descriptor: ActorDescriptor,
        environment: ResidentEnvironment<H, O>,
        outcome: ResidentOutcome,
    ) -> Self {
        Self {
            descriptor,
            environment,
            boot: Some(ResidentBoot::Prepared(Box::new(outcome))),
            standing: ResidentStanding::Boot,
            session: None,
            shutdown_hook: None,
            launch_worktrees: Vec::new(),
            policy_installed: false,
            activation_wakes: None,
            owns_worker_runtime: true,
        }
    }

    fn child(
        descriptor: ActorDescriptor,
        environment: ResidentEnvironment<H, O>,
        entry: RootCustody,
        launch_worktrees: Vec<String>,
    ) -> Self {
        Self {
            descriptor,
            environment,
            boot: Some(ResidentBoot::Entry(entry)),
            standing: ResidentStanding::Boot,
            session: None,
            shutdown_hook: None,
            launch_worktrees,
            policy_installed: false,
            activation_wakes: None,
            owns_worker_runtime: false,
        }
    }

    fn context(&self, actor: ActorRef) -> ActorSessionContext {
        self.descriptor.session_context(actor)
    }

    fn failure(error: impl std::fmt::Display) -> KernelBehaviorError {
        KernelBehaviorError {
            detail: error.to_string(),
        }
    }

    fn invocation_failure(
        actor: ActorRef,
        error: impl std::fmt::Display,
    ) -> KernelInvocationFailure {
        KernelInvocationFailure::Failed {
            actor,
            detail: error.to_string(),
        }
    }

    /// Publish ordinary actor applications immediately, but hold worker
    /// applications until the owning root has attached the exact child to its
    /// reserved ledger entry. This makes external launch failure impossible
    /// to race ahead of typed worker correlation.
    fn publish_installation(&self, installation: LocalResidentInstallation) {
        if installation.launch_worktrees.is_empty() {
            let _ = self
                .environment
                .deployments
                .send(LocalResidentDeployment::PolicyInstalled(installation));
        } else {
            self.environment
                .pending_worker_installations
                .lock()
                .insert(installation.actor.identity(), installation);
        }
    }

    fn publish_retired(&self, actor: ActorRef, terminal: ActorTerminal) {
        if self.environment.retired.lock().insert(actor) {
            let _ = self
                .environment
                .deployments
                .send(LocalResidentDeployment::Retired { actor, terminal });
        }
    }
}

impl<H, O> ResidentKernelBehavior<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    async fn resolve_completion(
        &mut self,
        completion: crate::ResidentCompletion,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        let session = self.session.as_ref().ok_or_else(|| {
            ResidentActorWorkbenchError::ActorProtocol(
                "resident actor has no model transcript".into(),
            )
        })?;
        let mut admitted = session.begin_agent_session();
        self.environment
            .completions
            .resolve_admitted(
                &mut admitted,
                self.environment.provider.as_ref(),
                completion,
                self.environment.sink.clone(),
            )
            .await
            .map_err(|error| ResidentActorWorkbenchError::ActorProtocol(error.to_string()))
    }

    async fn start_child(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        start: crate::ResidentActorStart,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        let (descriptor, parent_hole, entry, launch_worktrees) = start.into_parts();
        if !self
            .descriptor
            .profile()
            .permits_child(descriptor.profile())
        {
            return Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                "actor profile {:?} cannot start child profile {:?}",
                self.descriptor.profile(),
                descriptor.profile()
            )));
        }
        if descriptor.placement().session != context.placement.session {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "child actor entry crossed a resident machine boundary".into(),
            ));
        }
        let label = descriptor.label().to_owned();
        let child = kernel
            .spawn_child(
                Some(label),
                Self::child(
                    descriptor,
                    self.environment.clone(),
                    entry,
                    launch_worktrees,
                ),
            )
            .await
            .map_err(|error| ResidentActorWorkbenchError::ActorProtocol(error.to_string()))?;
        self.environment
            .runner
            .resume_starting_parent(context.clone(), parent_hole, child.identity())
            .await
    }

    async fn resolve_outbound(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        ancestry: &crate::CallAncestry,
        outbound: ResidentOutbound,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        match outbound {
            ResidentOutbound::Cast {
                target,
                continuation,
                request,
            } => {
                let target_ref = kernel.resolve(target).ok_or_else(|| {
                    ResidentActorWorkbenchError::ActorProtocol(
                        KernelCallFailure::TargetUnavailable(target).to_string(),
                    )
                })?;
                let target_context = kernel.session_context(target).ok_or_else(|| {
                    ResidentActorWorkbenchError::ActorProtocol(
                        KernelCallFailure::TargetUnavailable(target).to_string(),
                    )
                })?;
                if target_context.placement.session != context.placement.session {
                    return Err(ResidentActorWorkbenchError::ActorProtocol(
                        KernelCallFailure::MachineBoundary {
                            caller: context.actor,
                            caller_session: context.placement.session,
                            target,
                            target_session: target_context.placement.session,
                        }
                        .to_string(),
                    ));
                }
                let request = self
                    .environment
                    .runner
                    .rehome_mailbox_value(
                        context.clone(),
                        request,
                        target_context.placement.resource_scope,
                    )
                    .await?;
                target_ref
                    .address()
                    .send_message(KernelMessage::Cast {
                        sender: context.actor,
                        request,
                    })
                    .map_err(|_| {
                        ResidentActorWorkbenchError::ActorProtocol(
                            KernelCallFailure::TargetExited(target).to_string(),
                        )
                    })?;
                self.environment
                    .runner
                    .resume_unit(context.clone(), continuation)
                    .await
            }
            ResidentOutbound::Call {
                target,
                continuation,
                request,
            } => {
                ancestry.enter(target).map_err(|error| {
                    ResidentActorWorkbenchError::ActorProtocol(error.to_string())
                })?;
                let target_ref = kernel.resolve(target).ok_or_else(|| {
                    ResidentActorWorkbenchError::ActorProtocol(
                        KernelCallFailure::TargetUnavailable(target).to_string(),
                    )
                })?;
                let target_context = kernel.session_context(target).ok_or_else(|| {
                    ResidentActorWorkbenchError::ActorProtocol(
                        KernelCallFailure::TargetUnavailable(target).to_string(),
                    )
                })?;
                if target_context.placement.session != context.placement.session {
                    return Err(ResidentActorWorkbenchError::ActorProtocol(
                        KernelCallFailure::MachineBoundary {
                            caller: context.actor,
                            caller_session: context.placement.session,
                            target,
                            target_session: target_context.placement.session,
                        }
                        .to_string(),
                    ));
                }
                let request = self
                    .environment
                    .runner
                    .rehome_mailbox_value(
                        context.clone(),
                        request,
                        target_context.placement.resource_scope,
                    )
                    .await?;
                let reply = target_ref
                    .address()
                    .call(
                        |reply| KernelMessage::Call {
                            caller: context.actor,
                            ancestry: ancestry.clone(),
                            request,
                            reply,
                        },
                        None,
                    )
                    .await
                    .map_err(|_| {
                        ResidentActorWorkbenchError::ActorProtocol(
                            KernelCallFailure::TargetExited(target).to_string(),
                        )
                    })?
                    .success_or_else(|| {
                        ResidentActorWorkbenchError::ActorProtocol(
                            KernelCallFailure::TargetExited(target).to_string(),
                        )
                    })?
                    .map_err(|error| {
                        ResidentActorWorkbenchError::ActorProtocol(error.to_string())
                    })?;
                self.environment
                    .runner
                    .resume_live(context.clone(), continuation, reply.into_custody())
                    .await
            }
        }
    }

    async fn resolve_effect(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        ancestry: &crate::CallAncestry,
        boundary: ResidentActorBoundary,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        match boundary {
            ResidentActorBoundary::Deliberate(completion) => {
                self.resolve_completion(completion).await
            }
            ResidentActorBoundary::Start(start) => self.start_child(kernel, context, start).await,
            ResidentActorBoundary::Outbound(outbound) => {
                self.resolve_outbound(kernel, context, ancestry, outbound)
                    .await
            }
            ResidentActorBoundary::Wait(wait) => {
                let target = kernel.resolve(wait.target).ok_or_else(|| {
                    ResidentActorWorkbenchError::ActorProtocol(
                        KernelCallFailure::TargetUnavailable(wait.target).to_string(),
                    )
                })?;
                let target_context = kernel.session_context(wait.target).ok_or_else(|| {
                    ResidentActorWorkbenchError::ActorProtocol(
                        KernelCallFailure::TargetUnavailable(wait.target).to_string(),
                    )
                })?;
                if target_context.placement.session != context.placement.session {
                    return Err(ResidentActorWorkbenchError::ActorProtocol(
                        "awaitExit crossed a resident machine boundary".into(),
                    ));
                }
                let terminal = target.terminal().wait().await;
                self.environment
                    .runner
                    .resume_terminal(context.clone(), wait.continuation, terminal)
                    .await
            }
            ResidentActorBoundary::Poll(poll) => {
                let terminal = kernel
                    .resolve(poll.target)
                    .and_then(|target| target.terminal().get());
                self.environment
                    .runner
                    .resume_optional_terminal(context.clone(), poll.continuation, terminal)
                    .await
            }
            ResidentActorBoundary::Worker(request) => {
                self.resolve_worker(kernel, context, request).await
            }
            other => Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                "`{}` is not an active actor effect",
                other.operation()
            ))),
        }
    }

    async fn resolve_worker(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        request: crate::worker_runtime::ResidentWorkerRequest,
    ) -> Result<ResidentOutcome, ResidentActorWorkbenchError> {
        use crate::worker_runtime::ResidentWorkerRequest as Request;

        let protocol = |detail: String| ResidentActorWorkbenchError::ActorProtocol(detail);
        let (continuation, answer) = match request {
            Request::ReserveBatch {
                specs,
                continuation,
            } => (
                continuation,
                Some(
                    self.environment
                        .workers
                        .reserve_batch(context.actor, specs)
                        .map_err(protocol)?,
                ),
            ),
            Request::Attach {
                handle,
                actor,
                exit_ref,
                custody_realm,
                continuation,
            } => {
                if !kernel.owns_child(actor) {
                    return Err(protocol(format!(
                        "worker attachment target {actor:?} is not a direct child of {:?}",
                        context.actor
                    )));
                }
                let child = kernel.resolve(actor).ok_or_else(|| {
                    protocol(format!("worker attachment target {actor:?} is unavailable"))
                })?;
                let result = self
                    .environment
                    .workers
                    .attach(context.actor, handle, child, exit_ref, custody_realm)
                    .map_err(protocol)?;
                if let Some(installation) = self
                    .environment
                    .pending_worker_installations
                    .lock()
                    .remove(&actor)
                {
                    let _ = self
                        .environment
                        .deployments
                        .send(LocalResidentDeployment::PolicyInstalled(installation));
                }
                (continuation, Some(result))
            }
            Request::FailStart {
                handle,
                detail,
                continuation,
            } => (
                continuation,
                Some(
                    self.environment
                        .workers
                        .fail_start(context.actor, handle, detail)
                        .map_err(protocol)?,
                ),
            ),
            Request::List { continuation } => (
                continuation,
                Some(
                    self.environment
                        .workers
                        .list(context.actor)
                        .map_err(protocol)?,
                ),
            ),
            Request::Inspect {
                handles,
                continuation,
            } => (
                continuation,
                Some(
                    self.environment
                        .workers
                        .inspect(context.actor, handles)
                        .map_err(protocol)?,
                ),
            ),
            Request::BorrowExit {
                handle,
                continuation,
            } => {
                let exit_ref = self
                    .environment
                    .workers
                    .borrow_exit(context.actor, handle)
                    .map_err(protocol)?;
                return self
                    .environment
                    .runner
                    .resume_live_borrowed(context.clone(), continuation, exit_ref)
                    .await;
            }
            Request::Acknowledge {
                acknowledgements,
                continuation,
            } => {
                let (answer, released) = self
                    .environment
                    .workers
                    .acknowledge(context.actor, acknowledgements)
                    .map_err(protocol)?;
                for lease in released {
                    self.environment
                        .runner
                        .close_realm(context.clone(), lease.custody_realm())
                        .await?;
                }
                (continuation, Some(answer))
            }
            Request::SessionContext { continuation } => {
                if self.activation_wakes.is_none() {
                    self.activation_wakes = Some(
                        self.environment
                            .workers
                            .take_wakes(context.actor)
                            .map_err(protocol)?,
                    );
                }
                let wakes = self.activation_wakes.as_deref().unwrap_or_default();
                let value = serde_json::json!({
                    "workerWakes": wakes.iter().map(|wake| serde_json::json!({
                        "wakeEvent": { "lifecycleEventId": wake.event },
                        "wakeHandle": { "workerId": wake.handle.as_str() },
                    })).collect::<Vec<_>>()
                });
                (continuation, Some(value))
            }
        };
        match answer {
            Some(answer) => {
                self.environment
                    .runner
                    .resume_json(context.clone(), continuation, answer)
                    .await
            }
            None => {
                self.environment
                    .runner
                    .resume_unit(context.clone(), continuation)
                    .await
            }
        }
    }

    async fn stabilize_program(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        ancestry: &crate::CallAncestry,
        mut outcome: ResidentOutcome,
    ) -> Result<KernelStep<()>, ResidentActorWorkbenchError> {
        loop {
            match self
                .environment
                .runner
                .capture_boundary(context.clone(), outcome, context.placement.resource_scope)
                .await?
            {
                ResidentActorBoundary::Completed => {
                    self.standing = ResidentStanding::Terminal;
                    return Ok(KernelStep::Stop {
                        output: (),
                        terminal: completed_terminal(),
                    });
                }
                ResidentActorBoundary::Receive(receiver) => {
                    self.standing = ResidentStanding::Receiving(receiver);
                    return Ok(KernelStep::Continue(()));
                }
                ResidentActorBoundary::McpAwait(awaiting) => {
                    if !self.policy_installed {
                        let actor = kernel.resolve(context.actor).ok_or_else(|| {
                            ResidentActorWorkbenchError::ActorProtocol(
                                "local actor was absent from its routing directory".into(),
                            )
                        })?;
                        let policy: Arc<dyn ResidentMcpEndpoint> =
                            Arc::new(crate::resident_mcp::install_local_resident_mcp(
                                actor.clone(),
                                &awaiting,
                            ));
                        let installation = LocalResidentInstallation {
                            actor,
                            label: self.descriptor.label().to_owned(),
                            policy,
                            initial_user_message: awaiting.initial_user_message.clone(),
                            launch_worktrees: self.launch_worktrees.clone(),
                        };
                        self.publish_installation(installation);
                        self.policy_installed = true;
                    }
                    self.standing = ResidentStanding::Mcp(awaiting);
                    return Ok(KernelStep::Continue(()));
                }
                ResidentActorBoundary::AgentSession(session) => {
                    let (request, hole, input) = session.into_parts();
                    let workbench = self
                        .environment
                        .runner
                        .workbench(request.output_type.clone(), request.output_modules.clone());
                    workbench
                        .mount_named_input(
                            context.clone(),
                            "sessionInput",
                            request.input_type.clone(),
                            input,
                        )
                        .await?;
                    if !self.policy_installed {
                        let actor = kernel.resolve(context.actor).ok_or_else(|| {
                            ResidentActorWorkbenchError::ActorProtocol(
                                "local actor was absent from its routing directory".into(),
                            )
                        })?;
                        let policy: Arc<dyn ResidentMcpEndpoint> =
                            Arc::new(crate::ResidentInteractivePolicy::local(actor.clone()));
                        let installation = LocalResidentInstallation {
                            actor,
                            label: self.descriptor.label().to_owned(),
                            policy,
                            initial_user_message: request.initial_user_message.clone(),
                            launch_worktrees: self.launch_worktrees.clone(),
                        };
                        self.publish_installation(installation);
                        self.policy_installed = true;
                    }
                    self.standing = ResidentStanding::Interactive(
                        crate::interactive_session::ResidentInteractiveAwait { request, hole },
                    );
                    return Ok(KernelStep::Continue(()));
                }
                boundary => {
                    outcome = self
                        .resolve_effect(kernel, context, ancestry, boundary)
                        .await?;
                }
            }
        }
    }

    async fn initialize(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        boot: ResidentBoot,
    ) -> Result<KernelStep<()>, ResidentActorWorkbenchError> {
        if self.owns_worker_runtime {
            // The root may consult its activation context before reaching its
            // first interactive suspension, so authority must exist during
            // actor initialization rather than after readiness publication.
            self.environment.workers.install_root(context.actor);
        }
        let outcome = match boot {
            ResidentBoot::Prepared(outcome) => *outcome,
            ResidentBoot::Entry(entry) => {
                let mut outcome = self
                    .environment
                    .runner
                    .run_rooted_entry(context.clone(), entry, context.placement.resource_scope)
                    .await?;
                loop {
                    match self
                        .environment
                        .runner
                        .capture_startup_step(
                            context.clone(),
                            outcome,
                            context.placement.resource_scope,
                        )
                        .await?
                    {
                        ResidentActorStartupStep::InstallShutdown(shutdown) => {
                            if self.shutdown_hook.is_some() {
                                return Err(ResidentActorWorkbenchError::ActorProtocol(
                                    "actor installed its shutdown hook more than once".into(),
                                ));
                            }
                            let (continuation, hook) = shutdown.into_parts();
                            self.shutdown_hook = Some(hook);
                            outcome = self
                                .environment
                                .runner
                                .resume_unit(context.clone(), continuation)
                                .await?;
                        }
                        ResidentActorStartupStep::Deliberate(completion) => {
                            outcome = self.resolve_completion(completion).await?;
                        }
                        ResidentActorStartupStep::Ready(readiness) => {
                            break self
                                .environment
                                .runner
                                .resume_readiness(context.clone(), readiness)
                                .await?;
                        }
                    }
                }
            }
        };
        self.stabilize_program(
            kernel,
            context,
            &crate::CallAncestry::begin(context.actor),
            outcome,
        )
        .await
    }

    async fn run_receiver(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        caller: Option<ActorRef>,
        ancestry: &crate::CallAncestry,
        request: MailboxValue,
    ) -> Result<(Option<MailboxValue>, KernelStep<()>), ResidentActorWorkbenchError> {
        let receiver = match std::mem::replace(&mut self.standing, ResidentStanding::Boot) {
            ResidentStanding::Receiving(receiver) => receiver,
            standing => {
                self.standing = standing;
                return Err(ResidentActorWorkbenchError::ActorProtocol(
                    "actor has no installed mailbox receiver".into(),
                ));
            }
        };
        if request.session() != context.placement.session {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "mailbox value crossed a resident machine boundary".into(),
            ));
        }
        let request = self
            .environment
            .runner
            .rehome_mailbox_value(context.clone(), request, context.placement.resource_scope)
            .await?;
        let handler_realm = RealmId::fresh();
        let outcome = self
            .environment
            .runner
            .run_mailbox_handler(
                context.clone(),
                receiver.handler,
                request.into_custody(),
                handler_realm,
            )
            .await?;
        let reply = self
            .environment
            .runner
            .capture_kernel_value(
                context.clone(),
                outcome,
                ResidentKernelBoundary::Reply,
                receiver.site,
                handler_realm,
                context.placement.resource_scope,
            )
            .await?;
        let reply_continuation = reply.continuation;
        let reply = if let Some(caller) = caller {
            let caller_context = kernel.session_context(caller).ok_or_else(|| {
                ResidentActorWorkbenchError::ActorProtocol(
                    KernelCallFailure::TargetUnavailable(caller).to_string(),
                )
            })?;
            let value = MailboxValue::new(context.placement.session, reply.value);
            Some(
                self.environment
                    .runner
                    .rehome_mailbox_value(
                        context.clone(),
                        value,
                        caller_context.placement.resource_scope,
                    )
                    .await?,
            )
        } else {
            drop(reply.value);
            None
        };
        let outcome = self
            .environment
            .runner
            .resume_unit(context.clone(), reply_continuation)
            .await?;
        let next = self
            .environment
            .runner
            .capture_kernel_value(
                context.clone(),
                outcome,
                ResidentKernelBoundary::Continue,
                receiver.site,
                handler_realm,
                context.placement.resource_scope,
            )
            .await?;
        let handler_done = self
            .environment
            .runner
            .resume_unit(context.clone(), next.continuation)
            .await?;
        if !matches!(handler_done, ResidentOutcome::Completed { .. }) {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "mailbox handler continued after its private settlement protocol".into(),
            ));
        }
        self.environment
            .runner
            .close_realm(context.clone(), handler_realm)
            .await?;
        let program = self
            .environment
            .runner
            .resume_live(context.clone(), receiver.continuation, next.value)
            .await?;
        let step = self
            .stabilize_program(kernel, context, ancestry, program)
            .await?;
        Ok((reply, step))
    }

    async fn settle_fragment_effects(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        workbench: &crate::ResidentActorWorkbench<H, O>,
        mut fragment: ResidentWorkbenchFragment,
        mut outcome: ResidentOutcome,
    ) -> Result<ResidentWorkbenchStep, ResidentActorWorkbenchError> {
        loop {
            match workbench
                .settle_item(context.clone(), fragment, outcome)
                .await?
            {
                ResidentWorkbenchStep::Running {
                    fragment: next_fragment,
                    outcome: next,
                } => {
                    let boundary = self
                        .environment
                        .runner
                        .capture_boundary(context.clone(), *next, context.placement.resource_scope)
                        .await?;
                    outcome = self
                        .resolve_effect(
                            kernel,
                            context,
                            &crate::CallAncestry::begin(context.actor),
                            boundary,
                        )
                        .await?;
                    fragment = next_fragment;
                }
                settled => return Ok(settled),
            }
        }
    }

    async fn execute_workbench(
        &mut self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        request: WorkbenchRequest,
        awaiting: crate::interactive_session::ResidentInteractiveAwait,
    ) -> Result<KernelStep<WorkbenchResponse>, ResidentActorWorkbenchError> {
        let workbench = self
            .environment
            .runner
            .workbench(
                awaiting.request.output_type.clone(),
                awaiting.request.output_modules.clone(),
            )
            .with_json_input(
                request
                    .input
                    .as_ref()
                    .map(tidepool_runtime::session::normalize_workbench_input),
            );
        let mut receipts = Vec::new();
        for (index, source) in request.items.iter().cloned().enumerate() {
            let block = ParsedBlock {
                ordinal: index + 1,
                total: request.items.len(),
                source,
            };
            let mut step = workbench.begin_item(context.clone(), block).await?;
            if let ResidentWorkbenchStep::Running { fragment, outcome } = step {
                step = self
                    .settle_fragment_effects(kernel, context, &workbench, fragment, *outcome)
                    .await?;
            }
            match step {
                ResidentWorkbenchStep::Committed(output) => receipts.push(WorkbenchItemReceipt {
                    index,
                    status: WorkbenchItemStatus::Committed,
                    output,
                }),
                ResidentWorkbenchStep::Rejected(output) => {
                    receipts.push(WorkbenchItemReceipt {
                        index,
                        status: WorkbenchItemStatus::Rejected,
                        output,
                    });
                    self.standing = ResidentStanding::Interactive(awaiting);
                    return Ok(KernelStep::Continue(workbench_response(
                        WorkbenchRunStatus::Rejected,
                        receipts,
                        index,
                        request.items.len(),
                    )));
                }
                ResidentWorkbenchStep::Completed(answer) => {
                    self.activation_wakes = None;
                    let outcome = workbench
                        .resume_completion(context.clone(), awaiting.hole, answer)
                        .await?;
                    let program = self
                        .stabilize_program(
                            kernel,
                            context,
                            &crate::CallAncestry::begin(context.actor),
                            outcome,
                        )
                        .await?;
                    let response = workbench_response(
                        WorkbenchRunStatus::Completed,
                        receipts,
                        index + 1,
                        request.items.len(),
                    );
                    return Ok(match program {
                        KernelStep::Continue(()) => KernelStep::Continue(response),
                        KernelStep::Stop { terminal, .. } => KernelStep::Stop {
                            output: response,
                            terminal,
                        },
                    });
                }
                ResidentWorkbenchStep::Running { .. } => {
                    unreachable!("running workbench steps are settled above")
                }
            }
        }
        self.standing = ResidentStanding::Interactive(awaiting);
        Ok(KernelStep::Continue(workbench_response(
            WorkbenchRunStatus::Committed,
            receipts,
            request.items.len(),
            request.items.len(),
        )))
    }
}

impl<H, O> KernelBehavior for ResidentKernelBehavior<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    fn start<'a>(
        &'a mut self,
        kernel: &'a KernelContext,
    ) -> futures_util::future::BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>> {
        Box::pin(async move {
            let context = self.context(kernel.identity());
            kernel.install_session_context(context.clone())?;
            self.session = Some(ActorAgentSession::local(context.clone()));
            let boot = self.boot.take().ok_or_else(|| KernelBehaviorError {
                detail: "resident actor boot was consumed twice".into(),
            })?;
            self.initialize(kernel, &context, boot)
                .await
                .map_err(Self::failure)
        })
    }

    fn cast<'a>(
        &'a mut self,
        kernel: &'a KernelContext,
        _sender: ActorRef,
        request: MailboxValue,
    ) -> futures_util::future::BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>> {
        Box::pin(async move {
            let context = self.context(kernel.identity());
            let (_, step) = self
                .run_receiver(
                    kernel,
                    &context,
                    None,
                    &crate::CallAncestry::begin(context.actor),
                    request,
                )
                .await
                .map_err(Self::failure)?;
            Ok(step)
        })
    }

    fn call<'a>(
        &'a mut self,
        kernel: &'a KernelContext,
        caller: ActorRef,
        ancestry: crate::CallAncestry,
        request: MailboxValue,
    ) -> futures_util::future::BoxFuture<'a, Result<KernelStep<MailboxValue>, KernelBehaviorError>>
    {
        Box::pin(async move {
            let context = self.context(kernel.identity());
            let (reply, step) = self
                .run_receiver(kernel, &context, Some(caller), &ancestry, request)
                .await
                .map_err(Self::failure)?;
            let reply = reply.ok_or_else(|| KernelBehaviorError {
                detail: "synchronous mailbox handler produced no reply".into(),
            })?;
            Ok(match step {
                KernelStep::Continue(()) => KernelStep::Continue(reply),
                KernelStep::Stop { terminal, .. } => KernelStep::Stop {
                    output: reply,
                    terminal,
                },
            })
        })
    }

    fn mcp<'a>(
        &'a mut self,
        kernel: &'a KernelContext,
        name: String,
        arguments: serde_json::Value,
    ) -> futures_util::future::BoxFuture<
        'a,
        Result<KernelStep<serde_json::Value>, KernelInvocationFailure>,
    > {
        Box::pin(async move {
            let context = self.context(kernel.identity());
            let awaiting = match std::mem::replace(&mut self.standing, ResidentStanding::Boot) {
                ResidentStanding::Mcp(awaiting) => awaiting,
                standing => {
                    self.standing = standing;
                    return Err(KernelInvocationFailure::Rejected {
                        actor: context.actor,
                        detail: "actor has no installed MCP policy".into(),
                    });
                }
            };
            let mut outcome = self
                .environment
                .runner
                .resume_mcp_invocation(context.clone(), awaiting.continuation, name, arguments)
                .await
                .map_err(|error| Self::invocation_failure(context.actor, error))?;
            let mut result = None;
            loop {
                let boundary = self
                    .environment
                    .runner
                    .capture_boundary(context.clone(), outcome, context.placement.resource_scope)
                    .await
                    .map_err(|error| Self::invocation_failure(context.actor, error))?;
                match boundary {
                    ResidentActorBoundary::McpReply(reply) => {
                        if result.replace(reply.result).is_some() {
                            return Err(KernelInvocationFailure::Failed {
                                actor: context.actor,
                                detail: "actor MCP invocation replied more than once".into(),
                            });
                        }
                        outcome = self
                            .environment
                            .runner
                            .resume_unit(context.clone(), reply.continuation)
                            .await
                            .map_err(|error| Self::invocation_failure(context.actor, error))?;
                    }
                    ResidentActorBoundary::McpAwait(next) => {
                        let result = result.ok_or_else(|| KernelInvocationFailure::Failed {
                            actor: context.actor,
                            detail: "actor awaited another MCP invocation without replying".into(),
                        })?;
                        self.standing = ResidentStanding::Mcp(next);
                        return Ok(KernelStep::Continue(result));
                    }
                    ResidentActorBoundary::Completed => {
                        let result = result.ok_or_else(|| KernelInvocationFailure::Failed {
                            actor: context.actor,
                            detail: "actor completed an MCP invocation without replying".into(),
                        })?;
                        self.standing = ResidentStanding::Terminal;
                        return Ok(KernelStep::Stop {
                            output: result,
                            terminal: completed_terminal(),
                        });
                    }
                    boundary => {
                        outcome = self
                            .resolve_effect(
                                kernel,
                                &context,
                                &crate::CallAncestry::begin(context.actor),
                                boundary,
                            )
                            .await
                            .map_err(|error| Self::invocation_failure(context.actor, error))?;
                    }
                }
            }
        })
    }

    fn workbench<'a>(
        &'a mut self,
        kernel: &'a KernelContext,
        request: WorkbenchRequest,
    ) -> futures_util::future::BoxFuture<
        'a,
        Result<KernelStep<WorkbenchResponse>, KernelInvocationFailure>,
    > {
        Box::pin(async move {
            let context = self.context(kernel.identity());
            let awaiting = match std::mem::replace(&mut self.standing, ResidentStanding::Boot) {
                ResidentStanding::Interactive(awaiting) => awaiting,
                standing => {
                    self.standing = standing;
                    return Err(KernelInvocationFailure::Rejected {
                        actor: context.actor,
                        detail: "actor has no active Haskell agent session".into(),
                    });
                }
            };
            self.execute_workbench(kernel, &context, request, awaiting)
                .await
                .map_err(|error| Self::invocation_failure(context.actor, error))
        })
    }

    fn external_application_failed(
        &mut self,
        _context: &KernelContext,
        _failure: ExternalApplicationFailure,
    ) -> futures_util::future::BoxFuture<'_, ExternalFailureDisposition> {
        Box::pin(async { ExternalFailureDisposition::Applied })
    }

    fn shutdown<'a>(
        &'a mut self,
        kernel: &'a KernelContext,
        terminal: &'a ActorTerminal,
    ) -> futures_util::future::BoxFuture<'a, Result<(), KernelBehaviorError>> {
        Box::pin(async move {
            let context = self.context(kernel.identity());
            if let Some(hook) = self.shutdown_hook.take() {
                self.environment
                    .runner
                    .run_shutdown(
                        context.clone(),
                        hook,
                        context.placement.resource_scope,
                        terminal.kind,
                        std::time::Duration::from_secs(30),
                    )
                    .await
                    .map_err(Self::failure)?;
            }
            self.environment
                .runner
                .close_realm(context, self.descriptor.placement().resource_scope)
                .await
                .map_err(Self::failure)?;
            self.standing = ResidentStanding::Terminal;
            Ok(())
        })
    }

    fn stopped<'a>(
        &'a mut self,
        kernel: &'a KernelContext,
        terminal: &'a ActorTerminal,
    ) -> futures_util::future::BoxFuture<'a, ()> {
        Box::pin(async move {
            self.publish_retired(kernel.identity(), terminal.clone());
        })
    }

    fn child_exited(&mut self, notice: ChildExitNotice) -> futures_util::future::BoxFuture<'_, ()> {
        Box::pin(async move {
            self.publish_retired(notice.child.identity(), notice.terminal.clone());
            let worker_wake = self
                .environment
                .workers
                .child_exited(notice.child.identity());
            let _ = self
                .environment
                .deployments
                .send(LocalResidentDeployment::ChildExited {
                    notice,
                    worker_wake,
                });
        })
    }
}

pub async fn spawn_resident_root<H, O>(
    source: ActorWorkbenchSource,
    provider: Arc<dyn DynModelProvider>,
    sink: Option<StreamSink>,
    root: ResidentActorRoot<H, O>,
) -> Result<
    (
        LocalActorRef,
        ractor::concurrency::JoinHandle<()>,
        mpsc::UnboundedReceiver<LocalResidentDeployment>,
    ),
    ractor::SpawnErr,
>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    let (descriptor, machine, outcome) = root.into_parts();
    let machines = Arc::new(ActorMachineRegistry::<H, O>::new());
    let session = descriptor.placement().session;
    debug_assert!(machines.insert_idle(session, machine).is_none());
    let runner = ResidentActorRunner::new(Arc::clone(&machines), source.clone());
    let completions = ResidentCompletionExecutor::new(machines, source);
    let (deployments, receiver) = mpsc::unbounded_channel();
    let workers = Arc::new(crate::worker_runtime::WorkerRuntime::default());
    let environment = ResidentEnvironment {
        runner,
        completions,
        provider,
        sink,
        deployments,
        workers: Arc::clone(&workers),
        pending_worker_installations: Arc::new(Mutex::new(std::collections::HashMap::new())),
        retired: Arc::new(Mutex::new(std::collections::HashSet::new())),
    };
    let name = Some(descriptor.label().to_owned());
    let behavior = ResidentKernelBehavior::prepared(descriptor, environment, outcome);
    let (actor, task) = crate::spawn_local_actor(name, behavior).await?;
    Ok((actor, task, receiver))
}

fn completed_terminal() -> ActorTerminal {
    ActorTerminal {
        kind: ActorExitKind::Completed,
        summary: "completed".into(),
    }
}

fn workbench_response(
    status: WorkbenchRunStatus,
    items: Vec<WorkbenchItemReceipt>,
    next_index: usize,
    total: usize,
) -> WorkbenchResponse {
    WorkbenchResponse {
        status,
        items,
        next_index,
        total,
    }
}
