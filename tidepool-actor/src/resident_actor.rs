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
    ResidentCompletionExecutor, ResidentToolEndpoint,
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
    pub policy: Arc<dyn ResidentToolEndpoint>,
    pub initial_user_message: Option<String>,
    pub launch_worktrees: Vec<String>,
}

#[derive(Clone)]
pub enum LocalResidentDeployment {
    PolicyInstalled(LocalResidentInstallation),
    /// A resident program opened another typed session in an already-running
    /// interactive application. The message is an ordinary User activation;
    /// the live value itself is mounted as `sessionInput` in Haskell.
    SessionReady {
        activation: crate::ResidentActivation,
    },
    ChildExited {
        notice: ChildExitNotice,
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
    Tools(crate::resident_tools::ResidentToolAwait),
    Interactive(crate::interactive_session::ResidentInteractiveAwait),
    Terminal,
}

#[derive(Default)]
struct ObservedChildExits(std::collections::HashSet<ActorRef>);

impl ObservedChildExits {
    fn record(&mut self, child: ActorRef) {
        self.0.insert(child);
    }

    fn take(&mut self, child: ActorRef) -> bool {
        self.0.remove(&child)
    }
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
    pending_program: Option<ResidentOutcome>,
    observed_child_exits: ObservedChildExits,
    deferred_child_failures: Vec<ChildExitNotice>,
    next_activation_sequence: u64,
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
            pending_program: None,
            observed_child_exits: ObservedChildExits::default(),
            deferred_child_failures: Vec::new(),
            next_activation_sequence: 1,
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
            pending_program: None,
            observed_child_exits: ObservedChildExits::default(),
            deferred_child_failures: Vec::new(),
            next_activation_sequence: 1,
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

    /// Publish an installed actor application exactly once readiness has made
    /// its reference usable.
    fn publish_installation(&self, installation: LocalResidentInstallation) {
        let _ = self
            .environment
            .deployments
            .send(LocalResidentDeployment::PolicyInstalled(installation));
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
        let child = kernel
            .spawn_child(
                None,
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
                self.observed_child_exits.record(wait.target);
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
                let observed = terminal.is_some();
                let outcome = self
                    .environment
                    .runner
                    .resume_optional_terminal(context.clone(), poll.continuation, terminal)
                    .await?;
                if observed {
                    self.observed_child_exits.record(poll.target);
                }
                Ok(outcome)
            }
            other => Err(ResidentActorWorkbenchError::ActorProtocol(format!(
                "`{}` is not an active actor effect",
                other.operation()
            ))),
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
                ResidentActorBoundary::ToolAwait(awaiting) => {
                    if !self.policy_installed {
                        let actor = kernel.resolve(context.actor).ok_or_else(|| {
                            ResidentActorWorkbenchError::ActorProtocol(
                                "local actor was absent from its routing directory".into(),
                            )
                        })?;
                        let policy: Arc<dyn ResidentToolEndpoint> =
                            Arc::new(crate::resident_tools::install_local_resident_tools(
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
                    self.standing = ResidentStanding::Tools(awaiting);
                    return Ok(KernelStep::Continue(()));
                }
                ResidentActorBoundary::AgentSession(session) => {
                    let (request, hole, input) = session.into_parts();
                    let activation_reason = request.activation_reason;
                    let workbench = self
                        .environment
                        .runner
                        .workbench(request.completion.clone(), request.output_modules.clone());
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
                        let policy: Arc<dyn ResidentToolEndpoint> =
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
                    if let Some(activation) = crate::ResidentActivation::mounted(
                        context.actor,
                        self.next_activation_sequence,
                        activation_reason,
                        match &self.standing {
                            ResidentStanding::Interactive(awaiting) => {
                                awaiting.request.input_type.clone()
                            }
                            _ => unreachable!(),
                        },
                    ) {
                        self.next_activation_sequence += 1;
                        let _ = self
                            .environment
                            .deployments
                            .send(LocalResidentDeployment::SessionReady { activation });
                    }
                    if activation_reason == crate::ActivationReason::ManualReady {
                        for notice in self.deferred_child_failures.drain(..) {
                            if !self.observed_child_exits.take(notice.child.identity()) {
                                let _ = self
                                    .environment
                                    .deployments
                                    .send(LocalResidentDeployment::ChildExited { notice });
                            }
                        }
                    }
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
    ) -> Result<KernelStep<WorkbenchResponse>, ResidentActorWorkbenchError> {
        let ResidentStanding::Interactive(awaiting_view) = &self.standing else {
            return Err(ResidentActorWorkbenchError::ActorProtocol(
                "interactive workbench lost its agent-session continuation".into(),
            ));
        };
        let workbench = self
            .environment
            .runner
            .workbench(
                awaiting_view.request.completion.clone(),
                awaiting_view.request.output_modules.clone(),
            )
            .with_json_input(
                request
                    .input
                    .as_ref()
                    .map(tidepool_runtime::session::normalize_workbench_input),
            );
        let mut receipts = Vec::new();
        let mut index = 0;
        while index < request.items.len() {
            if let Ok(Some(first)) =
                workbench.inspection_query(&request.items[index], request.input_kind(index))
            {
                let mut queries = vec![first];
                while index + queries.len() < request.items.len() {
                    let candidate = index + queries.len();
                    match workbench
                        .inspection_query(&request.items[candidate], request.input_kind(candidate))
                    {
                        Ok(Some(query)) => queries.push(query),
                        Ok(None) | Err(_) => break,
                    }
                }
                let batch_len = queries.len();
                let outputs = workbench.inspect_items(context.clone(), queries).await?;
                for (offset, output) in outputs.into_iter().enumerate() {
                    let receipt_index = index + offset;
                    match output {
                        Ok(output) => receipts.push(WorkbenchItemReceipt {
                            index: receipt_index,
                            status: WorkbenchItemStatus::Committed,
                            output,
                        }),
                        Err(output) => {
                            receipts.push(WorkbenchItemReceipt {
                                index: receipt_index,
                                status: WorkbenchItemStatus::Rejected,
                                output,
                            });
                            return Ok(KernelStep::Continue(workbench_response(
                                WorkbenchRunStatus::Rejected,
                                receipts,
                                receipt_index,
                                request.items.len(),
                            )));
                        }
                    }
                }
                index += batch_len;
                continue;
            }

            let source = request.items[index].clone();
            let block = ParsedBlock {
                ordinal: index + 1,
                total: request.items.len(),
                source,
            };
            let mut step = workbench
                .begin_item(context.clone(), block, request.input_kind(index))
                .await?;
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
                    return Ok(KernelStep::Continue(workbench_response(
                        WorkbenchRunStatus::Rejected,
                        receipts,
                        index,
                        request.items.len(),
                    )));
                }
                ResidentWorkbenchStep::Completed(answer) => {
                    let awaiting =
                        match std::mem::replace(&mut self.standing, ResidentStanding::Boot) {
                            ResidentStanding::Interactive(awaiting) => awaiting,
                            standing => {
                                self.standing = standing;
                                return Err(ResidentActorWorkbenchError::ActorProtocol(
                                    "completion lost its agent-session continuation".into(),
                                ));
                            }
                        };
                    let outcome = workbench
                        .resume_completion(context.clone(), awaiting.hole, answer)
                        .await?;
                    if self.pending_program.replace(outcome).is_some() {
                        return Err(ResidentActorWorkbenchError::ActorProtocol(
                            "actor yielded a second continuation before resuming the first".into(),
                        ));
                    }
                    let response = workbench_response(
                        WorkbenchRunStatus::Completed,
                        receipts,
                        index + 1,
                        request.items.len(),
                    );
                    return Ok(KernelStep::ContinueLater(response));
                }
                ResidentWorkbenchStep::Running { .. } => {
                    unreachable!("running workbench steps are settled above")
                }
            }
            index += 1;
        }
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
                KernelStep::ContinueLater(()) => KernelStep::ContinueLater(reply),
                KernelStep::Stop { terminal, .. } => KernelStep::Stop {
                    output: reply,
                    terminal,
                },
            })
        })
    }

    fn tool<'a>(
        &'a mut self,
        kernel: &'a KernelContext,
        invocation: tidepool_tool::ToolInvocation,
    ) -> futures_util::future::BoxFuture<
        'a,
        Result<KernelStep<serde_json::Value>, KernelInvocationFailure>,
    > {
        Box::pin(async move {
            let context = self.context(kernel.identity());
            let awaiting = match std::mem::replace(&mut self.standing, ResidentStanding::Boot) {
                ResidentStanding::Tools(awaiting) => awaiting,
                standing => {
                    self.standing = standing;
                    return Err(KernelInvocationFailure::Rejected {
                        actor: context.actor,
                        detail: "actor has no installed tool policy".into(),
                    });
                }
            };
            let tidepool_tool::ToolArguments::Structured(arguments) = invocation.arguments else {
                self.standing = ResidentStanding::Tools(awaiting);
                return Err(KernelInvocationFailure::Rejected {
                    actor: context.actor,
                    detail: "actor function tool received raw arguments".into(),
                });
            };
            let mut outcome = self
                .environment
                .runner
                .resume_tool_invocation(
                    context.clone(),
                    awaiting.continuation,
                    invocation.name,
                    arguments,
                )
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
                    ResidentActorBoundary::ToolReply(reply) => {
                        if result.replace(reply.result).is_some() {
                            return Err(KernelInvocationFailure::Failed {
                                actor: context.actor,
                                detail: "actor tool invocation replied more than once".into(),
                            });
                        }
                        outcome = self
                            .environment
                            .runner
                            .resume_unit(context.clone(), reply.continuation)
                            .await
                            .map_err(|error| Self::invocation_failure(context.actor, error))?;
                    }
                    ResidentActorBoundary::ToolAwait(next) => {
                        let result = result.ok_or_else(|| KernelInvocationFailure::Failed {
                            actor: context.actor,
                            detail: "actor awaited another tool invocation without replying".into(),
                        })?;
                        self.standing = ResidentStanding::Tools(next);
                        return Ok(KernelStep::Continue(result));
                    }
                    ResidentActorBoundary::Completed => {
                        let result = result.ok_or_else(|| KernelInvocationFailure::Failed {
                            actor: context.actor,
                            detail: "actor completed a tool invocation without replying".into(),
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
            if !matches!(self.standing, ResidentStanding::Interactive(_)) {
                return Err(KernelInvocationFailure::Rejected {
                    actor: context.actor,
                    detail: "actor has no active Haskell agent session".into(),
                });
            }
            self.execute_workbench(kernel, &context, request)
                .await
                .map_err(|error| Self::invocation_failure(context.actor, error))
        })
    }

    fn resume<'a>(
        &'a mut self,
        kernel: &'a KernelContext,
    ) -> futures_util::future::BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>> {
        Box::pin(async move {
            let context = self.context(kernel.identity());
            let outcome = self
                .pending_program
                .take()
                .ok_or_else(|| KernelBehaviorError {
                    detail: "resident actor resumed without a pending Haskell action".into(),
                })?;
            self.stabilize_program(
                kernel,
                &context,
                &crate::CallAncestry::begin(context.actor),
                outcome,
            )
            .await
            .map_err(Self::failure)
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
            let child = notice.child.identity();
            self.publish_retired(child, notice.terminal.clone());
            if self.observed_child_exits.take(child)
                || notice.terminal.kind == ActorExitKind::Completed
            {
                return;
            }
            if matches!(self.standing, ResidentStanding::Boot) {
                self.deferred_child_failures.push(notice);
            } else {
                let _ = self
                    .environment
                    .deployments
                    .send(LocalResidentDeployment::ChildExited { notice });
            }
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
    let environment = ResidentEnvironment {
        runner,
        completions,
        provider,
        sink,
        deployments,
        retired: Arc::new(Mutex::new(std::collections::HashSet::new())),
    };
    let behavior = ResidentKernelBehavior::prepared(descriptor, environment, outcome);
    let (actor, task) = crate::spawn_local_actor(None, behavior).await?;
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

#[cfg(test)]
mod tests {
    use super::ObservedChildExits;
    use crate::{ActorId, ActorRef, Incarnation};

    #[test]
    fn child_exit_observation_is_exact_and_consumed_once() {
        let observed = ActorRef {
            id: ActorId(7),
            incarnation: Incarnation(1),
        };
        let replacement = ActorRef {
            id: ActorId(7),
            incarnation: Incarnation(2),
        };
        let mut exits = ObservedChildExits::default();

        exits.record(observed);

        assert!(!exits.take(replacement));
        assert!(exits.take(observed));
        assert!(!exits.take(observed));
    }
}
