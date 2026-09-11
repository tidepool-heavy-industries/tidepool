//! Exact resident cleanup and HTTP drain remain separate from native/external work.
use super::*;
use crate::host_dynamic_tools::HostDynamicToolService;
use futures_util::future::BoxFuture;
use tidepool_actor::{HostedWorkSeal, ResidentCleanupOutcome, ResidentShutdown};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompletionBoundary {
    AwaitingNativeDecision,
    AbortForShutdown,
}
pub(super) type HostedOwner = Arc<tokio::sync::Mutex<HostedRetirement>>;
pub(super) type HostedSlot = Arc<Mutex<Option<HostedOwner>>>;

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
        seal: SealObservation,
        resident: ResidentObservation,
        http: HttpObservation,
        // These domains never constitute native/external cleanup evidence.
    },
}

enum EndpointSource {
    Canonical(Vec<tidepool_tool::HostedTool>),
    #[cfg(test)]
    Untrusted(Arc<dyn tidepool_actor::ResidentToolEndpoint>),
}

pub(super) struct HostedRetirement {
    endpoint_source: EndpointSource,
    actor: LocalActorRef,
    pub(super) control: crate::host_dynamic_tools::HostToolControl,
    boundary: CompletionBoundary,
    terminal_path: bool,
    seal: Option<Operation<HostedWorkSeal>>,
    shutdown: Option<Operation<ResidentShutdown>>,
    resident: ResidentObservation,
    service: Option<tokio::task::JoinHandle<Result<(), String>>>,
    service_result: Option<Result<(), String>>,
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
) -> Result<HostedOwner, String> {
    start_endpoint(
        slot,
        actor,
        binding_path,
        expected_resume,
        listener,
        EndpointSource::Canonical(Vec::new()),
        None,
    )
}

// Arbitrary endpoint decorators are a negative-test seam, never a production
// identity capability. They must obtain an exact seal even for terminal actors.
#[cfg(test)]
fn start_untrusted(
    slot: &HostedSlot,
    actor: LocalActorRef,
    endpoint: Arc<dyn tidepool_actor::ResidentToolEndpoint>,
    binding_path: PathBuf,
    listener: tokio::net::UnixListener,
) -> Result<HostedOwner, String> {
    start_endpoint(
        slot,
        actor,
        binding_path,
        None,
        listener,
        EndpointSource::Untrusted(endpoint),
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
        Arc<tidepool_node::command_resources::CommandResourceClient>,
        String,
    )>,
) -> Result<HostedOwner, String> {
    let endpoint: Arc<dyn tidepool_actor::ResidentToolEndpoint> = match &endpoint_source {
        EndpointSource::Canonical(tools) => {
            Arc::new(tidepool_actor::ResidentInteractivePolicy::local_with_tools(
                actor.clone(),
                tools.clone(),
            ))
        }
        #[cfg(test)]
        EndpointSource::Untrusted(endpoint) => endpoint.clone(),
    };
    let server = HostDynamicToolService::new(endpoint, binding_path, expected_resume)?
        .with_command_resources(resources);
    let mut entry = slot.lock();
    if entry.is_some() {
        return Err("host service already installed".into());
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
        seal: None,
        shutdown: None,
        resident: ResidentObservation::Pending,
        service: Some(service),
        service_result: None,
    }));
    *entry = Some(owner.clone());
    start
        .send(())
        .map_err(|_| "service start receiver lost".to_string())?;
    Ok(owner)
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
            self.shutdown.as_mut().unwrap().finish().await;
            self.resident = match self.shutdown.as_ref().unwrap() {
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
    tools: Vec<tidepool_tool::HostedTool>,
    binding_path: PathBuf,
    expected_resume: Option<BackendThreadId>,
    listener: tokio::net::UnixListener,
    resources: Option<(
        Arc<tidepool_node::command_resources::CommandResourceClient>,
        String,
    )>,
) -> Result<HostedOwner, String> {
    start_endpoint(
        slot,
        actor,
        binding_path,
        expected_resume,
        listener,
        EndpointSource::Canonical(tools),
        resources,
    )
}
