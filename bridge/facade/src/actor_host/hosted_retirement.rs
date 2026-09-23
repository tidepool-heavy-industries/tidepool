//! Exact resident cleanup and HTTP drain remain separate from native/external work.
use super::*;
use crate::host_dynamic_tools::HostDynamicToolService;
use exomonad_actor::{HostedWorkSeal, ResidentCleanupOutcome, ResidentShutdown};
use exomonad_agent::{
    InputProducerControlOutcome, InputProducerId, InteractiveAgentBackend, QueueReadyThread,
};
use futures_util::future::BoxFuture;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompletionBoundary {
    AwaitingNativeDecision,
    AbortForShutdown,
}
pub(super) type HostedOwner = Arc<tokio::sync::Mutex<HostedRetirement>>;
pub(super) type HostedSlot = Arc<Mutex<Option<HostedOwner>>>;

/// The real failure modes of standing up and driving hosted retirement, as
/// distinct variants rather than rendered text. Callers that only propagate
/// the error keep using `Display`; callers that must branch on which failure
/// occurred match the variant instead of substring-matching rendered text.
#[derive(Debug, thiserror::Error)]
pub(crate) enum HostedRetirementError {
    #[error("host service already installed")]
    ServiceAlreadyInstalled,
    #[error("cannot start hosted tool service: {0}")]
    ServiceSetup(String),
    #[error("service start receiver lost")]
    ServiceStartReceiverLost,
    #[error("native input custody is already decided")]
    InputCustodyAlreadyDecided,
    #[error("native input producer seal is already retained")]
    InputSealAlreadyRetained,
}

enum Operation<T> {
    Pending(BoxFuture<'static, Result<T, String>>),
    Finished(Result<T, String>),
}
impl<T> Operation<T> {
    async fn finish(&mut self) {
        if let Self::Pending(future) = self {
            let result = future.await;
            *self = Self::Finished(result);
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum SealObservation {
    Pending,
    TerminalPath,
    Confirmed(HostedWorkSeal),
    Failed(String),
}
#[derive(Debug, Clone)]
pub(crate) enum InputSealObservation {
    Required,
    NoProducer,
    #[cfg(test)]
    TestExempt,
    Pending,
    Sealed,
    Rejected,
    Unconfirmed(String),
}

impl InputSealObservation {
    pub(super) fn confirms_retirement(&self) -> bool {
        match self {
            Self::NoProducer | Self::Sealed => true,
            #[cfg(test)]
            Self::TestExempt => true,
            Self::Required | Self::Pending | Self::Rejected | Self::Unconfirmed(_) => false,
        }
    }
}
#[derive(Debug, Clone)]
pub(crate) enum ResidentObservation {
    Pending,
    Absent,
    Foreign(ActorRef),
    Accounted(ResidentCleanupOutcome),
    Failed(String),
}
#[derive(Debug, Clone)]
pub(crate) enum HttpObservation {
    Pending,
    Drained,
    Failed(String),
}
#[derive(Debug, Clone)]
pub(crate) enum HostedObservation {
    Pending,
    Observed {
        input_seal: InputSealObservation,
        seal: SealObservation,
        resident: ResidentObservation,
        http: HttpObservation,
        // These domains never constitute native/external cleanup evidence.
    },
}

enum EndpointSource {
    Canonical(Vec<exomonad_tool::HostedTool>),
    #[cfg(test)]
    Untrusted(Arc<dyn exomonad_actor::ResidentToolEndpoint>),
}

pub(super) struct HostedRetirement {
    endpoint_source: EndpointSource,
    actor: LocalActorRef,
    pub(super) control: crate::host_dynamic_tools::HostToolControl,
    boundary: CompletionBoundary,
    terminal_path: bool,
    input_seal: InputSealState,
    seal: Option<Operation<HostedWorkSeal>>,
    shutdown: Option<Operation<ResidentShutdown>>,
    resident: ResidentObservation,
    service: Option<tokio::task::JoinHandle<Result<(), String>>>,
    service_result: Option<Result<(), String>>,
    // Counts consecutive `advance` calls that returned immediately because
    // `input_seal` is still `Required` (a terminal actor with no seal
    // installed). Used only to warn once when retirement is stuck on this.
    stalled_on_required_input_seal: u32,
    warned_stalled_input_seal: bool,
}

enum InputSealState {
    Required,
    NoProducer,
    Pending(Operation<InputProducerControlOutcome>),
    #[cfg(test)]
    TestExempt,
}

/// Construct the canonical policy from this exact actor inside the owning entry.
/// No independently supplied endpoint can authorize the terminal cleanup path.
#[cfg(test)]
pub(super) fn start(
    slot: &HostedSlot,
    actor: LocalActorRef,
    binding_path: PathBuf,
    expected_resume: Option<BackendThreadId>,
    listener: tokio::net::UnixListener,
) -> Result<HostedOwner, HostedRetirementError> {
    start_endpoint(
        slot,
        actor,
        binding_path,
        expected_resume,
        listener,
        EndpointSource::Canonical(Vec::new()),
        None,
        None,
    )
}

// Arbitrary endpoint decorators are a negative-test seam, never a production
// identity capability. They must obtain an exact seal even for terminal actors.
#[cfg(test)]
fn start_untrusted(
    slot: &HostedSlot,
    actor: LocalActorRef,
    endpoint: Arc<dyn exomonad_actor::ResidentToolEndpoint>,
    binding_path: PathBuf,
    listener: tokio::net::UnixListener,
) -> Result<HostedOwner, HostedRetirementError> {
    start_endpoint(
        slot,
        actor,
        binding_path,
        None,
        listener,
        EndpointSource::Untrusted(endpoint),
        None,
        None,
    )
}

/// Store control and the gated task before opening the service start gate.
/// Neither task nor endpoint holds a back-reference to the owner slot.
fn start_endpoint(
    slot: &HostedSlot,
    actor: LocalActorRef,
    binding_path: PathBuf,
    expected_resume: Option<BackendThreadId>,
    listener: tokio::net::UnixListener,
    endpoint_source: EndpointSource,
    resources: Option<(
        Arc<exomonad_node::command_resources::CommandResourceClient>,
        String,
    )>,
    operation_journal: Option<PathBuf>,
) -> Result<HostedOwner, HostedRetirementError> {
    let endpoint: Arc<dyn exomonad_actor::ResidentToolEndpoint> = match &endpoint_source {
        EndpointSource::Canonical(tools) => {
            Arc::new(exomonad_actor::ResidentInteractivePolicy::local_with_tools(
                actor.clone(),
                tools.clone(),
            ))
        }
        #[cfg(test)]
        EndpointSource::Untrusted(endpoint) => endpoint.clone(),
    };
    let require_operation_journal = expected_resume.is_some();
    let mut server = HostDynamicToolService::new(endpoint, binding_path, expected_resume)
        .map_err(HostedRetirementError::ServiceSetup)?
        .with_command_resources(resources);
    if let Some(path) = operation_journal {
        server = server
            .with_operation_journal(path, require_operation_journal)
            .map_err(HostedRetirementError::ServiceSetup)?;
    }
    let mut entry = slot.lock();
    if entry.is_some() {
        return Err(HostedRetirementError::ServiceAlreadyInstalled);
    }
    let control = server.control();
    let (start, ready) = oneshot::channel();
    let service = tokio::spawn(async move {
        ready
            .await
            .map_err(|_| "service start cancelled".to_string())?;
        server
            .serve(listener)
            .await
            .map_err(|error| error.to_string())
    });
    let owner = Arc::new(tokio::sync::Mutex::new(HostedRetirement {
        endpoint_source,
        actor,
        control,
        boundary: CompletionBoundary::AwaitingNativeDecision,
        terminal_path: false,
        // Native input custody is mandatory for every production endpoint.
        // Retirement cannot fail open if its composition owner forgets to
        // install the producer fence.
        input_seal: InputSealState::Required,
        seal: None,
        shutdown: None,
        resident: ResidentObservation::Pending,
        service: Some(service),
        service_result: None,
        stalled_on_required_input_seal: 0,
        warned_stalled_input_seal: false,
    }));
    *entry = Some(owner.clone());
    start
        .send(())
        .map_err(|_| HostedRetirementError::ServiceStartReceiverLost)?;
    Ok(owner)
}

/// Start the native producer fence inside the retained hosted-work owner.
///
/// Construction is synchronous with respect to owner state: cancellation can
/// drop a later observer, but cannot lose or recreate the exact seal operation.
pub(super) async fn begin_input_seal(
    owner: &HostedOwner,
    backend: Arc<dyn InteractiveAgentBackend>,
    thread: QueueReadyThread,
    producer: InputProducerId,
) -> Result<(), HostedRetirementError> {
    install_input_seal(
        owner,
        Box::pin(async move {
            backend
                .seal_input_producer(&thread, &producer)
                .await
                .map_err(|error| error.to_string())
        }),
    )
    .await
}

/// Settle a retained native input seal while its producer process is still
/// alive. Retirement stops the process next; a seal first attempted after that
/// can only fail to connect, which leaves every hosted cleanup domain
/// unconfirmed. A timeout leaves the operation retained for `observe`.
pub(super) async fn settle_input_seal(owner: &HostedOwner, timeout: Duration) {
    let _ = tokio::time::timeout(timeout, async {
        let mut state = owner.lock().await;
        if let InputSealState::Pending(operation) = &mut state.input_seal {
            operation.finish().await;
        }
    })
    .await;
}

/// Record the mutually exclusive pre-admission path. This is required when a
/// launch is cancelled before the host creates its durable producer identity;
/// absence is explicit rather than inferred from a missing operation.
pub(super) async fn confirm_no_input_producer(
    owner: &HostedOwner,
) -> Result<(), HostedRetirementError> {
    let mut state = owner.lock().await;
    if !matches!(state.input_seal, InputSealState::Required) {
        return Err(HostedRetirementError::InputCustodyAlreadyDecided);
    }
    state.input_seal = InputSealState::NoProducer;
    Ok(())
}

async fn install_input_seal(
    owner: &HostedOwner,
    operation: BoxFuture<'static, Result<InputProducerControlOutcome, String>>,
) -> Result<(), HostedRetirementError> {
    let mut state = owner.lock().await;
    if !matches!(state.input_seal, InputSealState::Required) {
        return Err(HostedRetirementError::InputSealAlreadyRetained);
    }
    state.input_seal = InputSealState::Pending(Operation::Pending(operation));
    Ok(())
}

#[cfg(test)]
async fn exempt_input_seal_for_fixture(owner: &HostedOwner) {
    let mut state = owner.lock().await;
    assert!(matches!(state.input_seal, InputSealState::Required));
    state.input_seal = InputSealState::TestExempt;
}

pub(super) fn service_finished(owner: &HostedOwner) -> bool {
    owner.try_lock().is_ok_and(|state| {
        state.service_result.is_some()
            || state
                .service
                .as_ref()
                .is_some_and(|task| task.is_finished())
    })
}

/// Cancellation/timeout drops only this waiter. Every polled operation and its
/// result remain in the original owner; no seal/shutdown/service retry is created.
pub(super) async fn observe(
    owner: &HostedOwner,
    boundary: CompletionBoundary,
    timeout: Duration,
) -> HostedObservation {
    tokio::time::timeout(timeout, async {
        let mut state = owner.lock().await;
        if boundary == CompletionBoundary::AbortForShutdown {
            state.boundary = boundary;
        }
        state.advance().await;
        state.observation()
    })
    .await
    .unwrap_or(HostedObservation::Pending)
}

fn account(expected: ActorRef, cleanup: Option<ResidentCleanupOutcome>) -> ResidentObservation {
    match cleanup {
        None => ResidentObservation::Absent,
        Some(cleanup) if cleanup.actor() != expected => {
            ResidentObservation::Foreign(cleanup.actor())
        }
        Some(cleanup) => ResidentObservation::Accounted(cleanup),
    }
}
impl HostedRetirement {
    async fn advance(&mut self) {
        let exact = self.actor.identity();
        match &mut self.input_seal {
            InputSealState::Required => {
                self.stalled_on_required_input_seal += 1;
                if self.stalled_on_required_input_seal > 3 && !self.warned_stalled_input_seal {
                    self.warned_stalled_input_seal = true;
                    tracing::warn!(
                        actor = ?exact,
                        stalled_advances = self.stalled_on_required_input_seal,
                        "hosted retirement blocked: input_seal is still Required (no seal installed) for this terminal actor"
                    );
                }
                return;
            }
            InputSealState::NoProducer => {}
            InputSealState::Pending(input_seal) => {
                input_seal.finish().await;
                if !matches!(
                    input_seal,
                    Operation::Finished(Ok(InputProducerControlOutcome::Applied))
                ) {
                    return;
                }
            }
            #[cfg(test)]
            InputSealState::TestExempt => {}
        }
        if self.seal.is_none() && !self.terminal_path {
            if matches!(self.endpoint_source, EndpointSource::Canonical(_))
                && self.actor.terminal().get().is_some()
            {
                self.terminal_path = true;
                self.control.quiesce();
            } else {
                let control = self.control.clone();
                // quiesce_and_seal changes HTTP admission synchronously, so call
                // it INSIDE the stored future, not before storing that future.
                self.seal = Some(Operation::Pending(Box::pin(async move {
                    control
                        .quiesce_and_seal(exact)
                        .await
                        .map_err(|error| error.to_string())
                })));
            }
        }
        // A failed endpoint barrier is not repaired by the expected actor later
        // terminating: the endpoint may belong to a different, still-live actor.
        // Keep the original failure and HTTP owner addressable instead.
        if matches!(&self.seal, Some(Operation::Finished(Err(_)))) {
            return;
        }
        // Only an actor already terminal before a barrier was created uses the
        // direct terminal path. A previously started barrier must keep its exact
        // outcome, even if the actor terminates while the waiter is absent.
        if self.terminal_path {
            self.control.quiesce();
        } else if let Some(seal) = &mut self.seal {
            seal.finish().await;
            if matches!(seal, Operation::Finished(Err(_))) {
                return;
            }
        }
        if self.boundary != CompletionBoundary::AbortForShutdown {
            return;
        }
        if self.shutdown.is_none() && self.actor.terminal().get().is_some() {
            self.resident = account(exact, self.actor.terminal().cleanup());
        } else {
            if self.shutdown.is_none()
                && !matches!(&self.seal, Some(Operation::Finished(Ok(seal))) if seal.actor() == exact)
            {
                return;
            }
            if self.shutdown.is_none() {
                let actor = self.actor.clone();
                self.shutdown = Some(Operation::Pending(Box::pin(async move {
                    actor
                        .shutdown_with_cleanup(ActorTerminal {
                            kind: ActorExitKind::Cancelled,
                            summary: "host selected completion abort for shutdown".into(),
                        })
                        .await
                        .map_err(|error| error.to_string())
                })));
            }
            #[allow(clippy::expect_used, reason = "just set above")]
            self.shutdown
                .as_mut()
                .expect("just set above")
                .finish()
                .await;
            #[allow(clippy::expect_used, reason = "just set above")]
            let shutdown = self.shutdown.as_ref().expect("just set above");
            self.resident = match shutdown {
                Operation::Finished(Ok(result)) => account(exact, Some(result.cleanup.clone())),
                Operation::Finished(Err(error)) => ResidentObservation::Failed(error.clone()),
                Operation::Pending(_) => unreachable!(),
            };
        }
        // Check every owned resident component; unsupported/absent/foreign
        // outcomes cannot become permission to claim HTTP drain or settlement.
        if !matches!(&self.resident, ResidentObservation::Accounted(cleanup)
            if cleanup.is_confirmed())
        {
            return;
        }
        self.control.drain();
        if self.service_result.is_none() {
            if let Some(task) = &mut self.service {
                let result = task
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|result| result);
                self.service_result = Some(result);
                self.service.take();
            }
        }
    }
    fn observation(&self) -> HostedObservation {
        HostedObservation::Observed {
            input_seal: match &self.input_seal {
                InputSealState::Required => InputSealObservation::Required,
                InputSealState::NoProducer => InputSealObservation::NoProducer,
                InputSealState::Pending(Operation::Pending(_)) => InputSealObservation::Pending,
                InputSealState::Pending(Operation::Finished(Ok(
                    InputProducerControlOutcome::Applied,
                ))) => InputSealObservation::Sealed,
                InputSealState::Pending(Operation::Finished(Ok(
                    InputProducerControlOutcome::Rejected,
                ))) => InputSealObservation::Rejected,
                InputSealState::Pending(Operation::Finished(Ok(
                    InputProducerControlOutcome::Unknown,
                ))) => InputSealObservation::Unconfirmed(
                    "native input producer seal outcome is unknown".into(),
                ),
                InputSealState::Pending(Operation::Finished(Err(error))) => {
                    InputSealObservation::Unconfirmed(error.clone())
                }
                #[cfg(test)]
                InputSealState::TestExempt => InputSealObservation::TestExempt,
            },
            seal: if self.terminal_path {
                SealObservation::TerminalPath
            } else {
                match &self.seal {
                    Some(Operation::Finished(Ok(seal))) => SealObservation::Confirmed(seal.clone()),
                    Some(Operation::Finished(Err(error))) => SealObservation::Failed(error.clone()),
                    _ => SealObservation::Pending,
                }
            },
            resident: self.resident.clone(),
            http: match &self.service_result {
                None => HttpObservation::Pending,
                Some(Ok(())) => HttpObservation::Drained,
                Some(Err(error)) => HttpObservation::Failed(error.clone()),
            },
        }
    }
}

#[cfg(test)]
mod tests;

pub(super) fn start_with_resources(
    slot: &HostedSlot,
    actor: LocalActorRef,
    tools: Vec<exomonad_tool::HostedTool>,
    binding_path: PathBuf,
    expected_resume: Option<BackendThreadId>,
    listener: tokio::net::UnixListener,
    resources: Option<(
        Arc<exomonad_node::command_resources::CommandResourceClient>,
        String,
    )>,
    operation_journal: PathBuf,
) -> Result<HostedOwner, HostedRetirementError> {
    start_endpoint(
        slot,
        actor,
        binding_path,
        expected_resume,
        listener,
        EndpointSource::Canonical(tools),
        resources,
        Some(operation_journal),
    )
}
