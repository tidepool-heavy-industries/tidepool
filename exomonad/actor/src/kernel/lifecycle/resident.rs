//! Startup and public-owner transitions owned by the incarnation coordinator.

use super::{LifecycleCommand, LifecycleHandle, LifecycleState};
use crate::initial_context::InitialContextEvent;
use crate::resident_actor::{ActorPublicOwnerPlane, RootStartupState, WorkbenchPublicOwner};
use crate::{ActorDescriptor, ActorSessionContext, InitialContextObservation, RootStartupIntent};
use futures_util::future::BoxFuture;
use ractor::{ActorRef as RactorRef, RpcReplyPort};
use std::collections::VecDeque;
use std::sync::Arc;
use tidepool_runtime::session::{DurablePublicOwnerPublication, PublicManifestCommit, RecoveryPublicOwner};

/// Only the named public-surface operations cross into resident machine work.
/// This service contains the runner, never the Forest or directory.
pub(crate) trait PublicOwnerPublisher: Send + Sync {
    fn initialize(&self, context: ActorSessionContext, owner: RecoveryPublicOwner)
        -> BoxFuture<'static, Result<DurablePublicOwnerPublication, String>>;
    fn transfer(&self, context: ActorSessionContext, predecessor: RecoveryPublicOwner,
        owner: RecoveryPublicOwner, authority: Arc<dyn tidepool_runtime::session::RecoverySuccessorAuthority>)
        -> BoxFuture<'static, Result<DurablePublicOwnerPublication, String>>;
}

#[derive(Clone)]
pub(crate) struct ResidentSnapshot {
    pub context: ActorSessionContext,
    pub descriptor: ActorDescriptor,
    pub public_owner: ActorPublicOwnerPlane,
    pub initial_context: Option<InitialContextObservation>,
    pub startup: Option<RootStartupState>,
    pub installed_policy: Option<crate::InstalledToolLease>,
    pub fresh_context_seed: Option<String>,
}

pub(super) struct ResidentState {
    current: ResidentSnapshot,
    journal: Option<Arc<crate::ActorRecoveryJournal>>,
    publisher: Arc<dyn PublicOwnerPublisher>,
    startup_intent: Option<RootStartupIntent>,
    pending: Option<(u64, Pending)>,
    queued: VecDeque<Request>,
    retirement: Option<crate::ActorTerminal>,
    retirement_recorded: bool,
}

impl ResidentState {
    pub(super) fn has_pending(&self) -> bool {
        self.pending.is_some() || !self.queued.is_empty()
            || (self.retirement.is_some() && !self.retirement_recorded)
    }
}

pub(super) fn snapshot(state: &ResidentState) -> ResidentSnapshot { state.current.clone() }

pub(super) fn native_readiness(state: &ResidentState)
    -> Result<Option<Arc<tidepool_runtime::session::RuntimeDurablePublicReadiness>>, String> {
    match &state.current.public_owner {
        ActorPublicOwnerPlane::Ephemeral(_) => Ok(None),
        ActorPublicOwnerPlane::DurablePending(_) => Err("durable actor public owner is pending".into()),
        ActorPublicOwnerPlane::DurableReady(owner) => owner.native_readiness()
            .map(Some).ok_or_else(|| "durable public owner has no native readiness".into()),
    }
}

pub(super) enum ResidentCommand {
    Configure {
        context: ActorSessionContext,
        descriptor: ActorDescriptor,
        journal: Option<Arc<crate::ActorRecoveryJournal>>,
        publisher: Arc<dyn PublicOwnerPublisher>,
        startup: Option<RootStartupIntent>,
        fresh_context_seed: Option<String>,
        reply: RpcReplyPort<Result<(), String>>,
    },
    PolicyInstalled(Option<crate::InstalledToolLease>),
    Request(Request),
    Completed { sequence: u64, outcome: Completion },
    Retire(crate::ActorTerminal),
}

pub(super) enum Request {
    Initial {
        event: InitialContextEvent,
        begin: bool,
        expected: Option<tidepool_runtime::session::WorkbenchExecutionId>,
        reply: RpcReplyPort<Result<InitialTransition, String>>,
    },
    AcknowledgeInitial {
        operation: tidepool_runtime::session::WorkbenchExecutionId,
        publication: crate::InitialContextPublication,
        reply: RpcReplyPort<Result<InitialTransition, String>>,
    },
    PublishOwner {
        predecessor: Option<(RecoveryPublicOwner, Arc<dyn tidepool_runtime::session::RecoverySuccessorAuthority>)>,
        reply: RpcReplyPort<Result<PublicManifestCommit, String>>,
    },
    ReleaseStartup { intent: RootStartupIntent, reply: RpcReplyPort<Result<(), String>> },
    ActivateStartup { reply: RpcReplyPort<Result<bool, String>> },
}

pub(crate) struct InitialTransition {
    pub observation: InitialContextObservation,
    pub admitted: bool,
}

enum Pending {
    Initial(RpcReplyPort<Result<InitialTransition, String>>),
    Owner(RpcReplyPort<Result<PublicManifestCommit, String>>),
    Release(RpcReplyPort<Result<(), String>>),
    Retirement,
}

pub(super) enum Completion {
    Initial(Result<InitialTransition, String>),
    Owner(Result<DurablePublicOwnerPublication, String>),
    Release(Result<(), String>),
    Retirement(Result<(), String>),
}

impl LifecycleHandle {
    pub(crate) async fn configure_resident(&self, context: ActorSessionContext,
        descriptor: ActorDescriptor, journal: Option<Arc<crate::ActorRecoveryJournal>>,
        publisher: Arc<dyn PublicOwnerPublisher>, startup: Option<RootStartupIntent>,
        fresh_context_seed: Option<String>) -> Result<(), String> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.address.cast(LifecycleCommand::Resident(Box::new(ResidentCommand::Configure {
            context, descriptor, journal, publisher, startup, fresh_context_seed, reply: reply.into(),
        }))).map_err(|error| error.to_string())?;
        answer.await.map_err(|_| "resident configuration owner ended".to_owned())?
    }

    pub(crate) fn policy_installed(&self, policy: Option<crate::InstalledToolLease>) {
        let _ = self.address.cast(LifecycleCommand::Resident(Box::new(ResidentCommand::PolicyInstalled(policy))));
    }

    pub(crate) fn retire_resident(&self, terminal: crate::ActorTerminal) {
        let _ = self.address.cast(LifecycleCommand::Resident(Box::new(ResidentCommand::Retire(terminal))));
    }

    pub(crate) async fn begin_initial_context(&self, event: InitialContextEvent) -> Result<InitialTransition, String> {
        self.initial(event, true, None).await
    }

    pub(crate) async fn change_initial_context(&self, event: InitialContextEvent,
        expected: tidepool_runtime::session::WorkbenchExecutionId) -> Result<InitialContextObservation, String> {
        self.initial(event, false, Some(expected)).await.map(|value| value.observation)
    }

    async fn initial(&self, event: InitialContextEvent, begin: bool,
        expected: Option<tidepool_runtime::session::WorkbenchExecutionId>) -> Result<InitialTransition, String> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.send_resident(Request::Initial { event, begin, expected, reply: reply.into() })?;
        answer.await.map_err(|_| "initial context owner ended before settlement".to_owned())?
    }

    pub(crate) async fn acknowledge_initial_context(&self, operation: tidepool_runtime::session::WorkbenchExecutionId,
        publication: crate::InitialContextPublication) -> Result<InitialContextObservation, String> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.send_resident(Request::AcknowledgeInitial { operation, publication, reply: reply.into() })?;
        answer.await.map_err(|_| "initial context owner ended before acknowledgment".to_owned())?
            .map(|value| value.observation)
    }

    pub(crate) async fn publish_public_owner(&self,
        predecessor: Option<(RecoveryPublicOwner, Arc<dyn tidepool_runtime::session::RecoverySuccessorAuthority>)>)
        -> Result<PublicManifestCommit, String> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.send_resident(Request::PublishOwner { predecessor, reply: reply.into() })?;
        answer.await.map_err(|_| "public owner ended before publication settlement".to_owned())?
    }

    pub(crate) async fn release_startup(&self, intent: RootStartupIntent) -> Result<(), String> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.send_resident(Request::ReleaseStartup { intent, reply: reply.into() })?;
        answer.await.map_err(|_| "root startup owner ended before release".to_owned())?
    }

    pub(crate) async fn activate_startup(&self) -> Result<bool, String> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.send_resident(Request::ActivateStartup { reply: reply.into() })?;
        answer.await.map_err(|_| "root startup owner ended before activation".to_owned())?
    }

    fn send_resident(&self, request: Request) -> Result<(), String> {
        self.address.cast(LifecycleCommand::Resident(Box::new(ResidentCommand::Request(request))))
            .map_err(|error| error.to_string())
    }
}

pub(super) fn handle(state: &mut LifecycleState, address: &RactorRef<LifecycleCommand>, command: ResidentCommand) {
    match command {
        ResidentCommand::Configure { context, descriptor, journal, publisher, startup, fresh_context_seed, reply } => {
            let result = if state.resident.is_some() || state.is_closed() || context.actor != state.identity
                || context.placement != descriptor.placement() {
                Err("resident configuration differs from its original incarnation".to_owned())
            } else {
                let owner = match descriptor.persistence_policy() {
                    crate::ActorPersistencePolicy::Ephemeral => WorkbenchPublicOwner::issue(&context, &descriptor, None)
                        .map(ActorPublicOwnerPlane::Ephemeral).map_err(|error| error.to_string()),
                    crate::ActorPersistencePolicy::Durable => descriptor.actor_path()
                        .and_then(|path| RecoveryPublicOwner::new(path, context.actor.incarnation.0))
                        .map(ActorPublicOwnerPlane::DurablePending)
                        .ok_or_else(|| "durable actor requires its canonical admitted path".into()),
                };
                owner.map(|public_owner| {
                    state.resident = Some(ResidentState {
                        current: ResidentSnapshot { context, descriptor, public_owner, initial_context: None,
                            startup: startup.as_ref().map(|_| RootStartupState::Pending), installed_policy: None,
                            fresh_context_seed },
                        journal, publisher, startup_intent: startup, pending: None, queued: VecDeque::new(),
                        retirement: None, retirement_recorded: false,
                    });
                })
            };
            state.publish();
            let _ = reply.send(result);
        }
        ResidentCommand::PolicyInstalled(policy) => {
            if !state.is_closed() {
                if let Some(resident) = &mut state.resident { resident.current.installed_policy = policy; }
            }
        }
        ResidentCommand::Request(request) => {
            let closed = state.is_closed();
            if let Some(resident) = &mut state.resident {
                let admitted_completion = match &request {
                    Request::Initial { begin: false, expected: Some(expected), event, .. } =>
                        matches!(event, InitialContextEvent::Retain { .. } | InitialContextEvent::Fail { .. }
                            | InitialContextEvent::Uncertain { .. })
                        && resident.current.initial_context.as_ref().is_some_and(|current|
                            matches!(current.state, crate::InitialContextState::Preparing)
                            && current.identity.as_ref().is_some_and(|identity| &identity.operation == expected)),
                    _ => false,
                };
                if !closed || admitted_completion { resident.queued.push_back(request); }
                else { refuse(request, "actor lifecycle admission is closed"); }
            } else { refuse(request, "resident lifecycle is not configured"); }
        }
        ResidentCommand::Retire(terminal) => {
            if let Some(resident) = &mut state.resident { resident.retirement = Some(terminal); }
        }
        ResidentCommand::Completed { sequence, outcome } => complete(state, sequence, outcome),
    }
    advance(state, address);
}

fn refuse(request: Request, detail: &str) {
    match request {
        Request::Initial { reply, .. } | Request::AcknowledgeInitial { reply, .. } => { let _ = reply.send(Err(detail.into())); }
        Request::PublishOwner { reply, .. } => { let _ = reply.send(Err(detail.into())); }
        Request::ReleaseStartup { reply, .. } => { let _ = reply.send(Err(detail.into())); }
        Request::ActivateStartup { reply } => { let _ = reply.send(Err(detail.into())); }
    }
}

fn complete(state: &mut LifecycleState, sequence: u64, outcome: Completion) {
    let Some(resident) = &mut state.resident else { return; };
    if resident.pending.as_ref().is_none_or(|(issued, _)| *issued != sequence) { return; }
    let (_, pending) = resident.pending.take().expect("matched resident operation");
    state.tasks.remove(&sequence);
    match (pending, outcome) {
        (Pending::Initial(reply), Completion::Initial(result)) => {
            let result = result.map(|transition| {
                resident.current.initial_context = Some(transition.observation.clone());
                transition
            });
            state.publish();
            let _ = reply.send(result);
        }
        (Pending::Owner(reply), Completion::Owner(result)) => {
            let result = result.and_then(|publication| match publication {
                DurablePublicOwnerPublication::Committed(readiness) => {
                    let owner = WorkbenchPublicOwner::issue(&resident.current.context, &resident.current.descriptor,
                        Some(readiness)).map_err(|error| error.to_string())?;
                    resident.current.public_owner = ActorPublicOwnerPlane::DurableReady(owner);
                    Ok(PublicManifestCommit::Durable)
                }
                DurablePublicOwnerPublication::Refused { error } => Ok(PublicManifestCommit::Refused { error }),
                DurablePublicOwnerPublication::RecoveryRequired { error } => Ok(PublicManifestCommit::RecoveryRequired { error }),
            });
            state.publish();
            let _ = reply.send(result);
        }
        (Pending::Release(reply), Completion::Release(result)) => {
            if result.is_ok() { resident.current.startup = Some(RootStartupState::Released); }
            state.publish();
            let _ = reply.send(result);
        }
        (Pending::Retirement, Completion::Retirement(result)) => {
            resident.retirement_recorded = true;
            if let Err(error) = result { tracing::error!(actor=?state.identity, %error, "actor retirement journal remains uncertain"); }
        }
        _ => unreachable!("resident completion kind differs from its issued pending operation"),
    }
}

fn advance(state: &mut LifecycleState, address: &RactorRef<LifecycleCommand>) {
    loop {
        let closed = state.is_closed();
        let Some(resident) = state.resident.as_mut() else { return; };
        if resident.pending.is_some() { return; }
        let Some(request) = resident.queued.pop_front() else {
            if !resident.retirement_recorded {
                if let Some(terminal) = resident.retirement.clone() {
                    let journal = resident.journal.clone();
                    let actor = state.identity;
                    let sequence = state.next_operation();
                    state.resident.as_mut().unwrap().pending = Some((sequence, Pending::Retirement));
                    spawn(state, address, sequence, Box::pin(async move {
                        Completion::Retirement(match journal { Some(journal) => journal.retire(actor, terminal).await.map_err(|error| error.to_string()), None => Ok(()) })
                    }));
                }
            }
            return;
        };
        if let Request::ActivateStartup { reply } = request {
            let result = match resident.current.startup {
                Some(RootStartupState::Released) if !closed => { resident.current.startup = Some(RootStartupState::Activated); Ok(true) },
                Some(RootStartupState::Activated) => Ok(false),
                _ => Err("root startup has not been durably released".into()),
            };
            state.publish(); let _ = reply.send(result); continue;
        }
        if closed && matches!(&request, Request::PublishOwner { .. } | Request::ReleaseStartup { .. }) {
            refuse(request, "actor closed before external publication started"); continue;
        }
        let current = resident.current.clone();
        let journal = resident.journal.clone();
        let publisher = Arc::clone(&resident.publisher);
        let intent = resident.startup_intent.clone();
        let sequence = state.next_operation();
        let (pending, work): (Pending, BoxFuture<'static, Completion>) = match request {
            Request::Initial { event, begin, expected, reply } => {
                if begin {
                    if let Some(observation) = current.initial_context {
                        let _ = reply.send(Ok(InitialTransition { observation, admitted: false })); continue;
                    }
                }
                let owner = current.initial_context.as_ref().and_then(|value| value.identity.as_ref())
                    .map_or(current.context.actor, |identity| identity.actor);
                (Pending::Initial(reply), Box::pin(async move {
                    let result = async {
                        if begin {
                            if let Some(journal) = &journal {
                                if let Some(observation) = journal.initial_context(owner).await.map_err(|error| error.to_string())? {
                                    return Ok(InitialTransition { observation, admitted: false });
                                }
                            }
                        } else if current.initial_context.as_ref().and_then(|value| value.identity.as_ref())
                            .is_none_or(|identity| Some(&identity.operation) != expected.as_ref()) {
                            return Err("initial context completion has another operation identity".into());
                        }
                        let observation = if let Some(journal) = journal {
                            journal.record_initial_context(owner, event).await.map_err(|error| error.to_string())?
                        } else {
                            let mut next = current.initial_context;
                            crate::initial_context::apply_event(&mut next, event).map_err(|error| error.to_string())?;
                            next.ok_or_else(|| "initial context transition produced no result".to_owned())?
                        };
                        Ok(InitialTransition { observation, admitted: true })
                    }.await;
                    Completion::Initial(result)
                }))
            }
            Request::AcknowledgeInitial { operation, publication, reply } => {
                (Pending::Initial(reply), Box::pin(async move {
                    let result = async {
                        let observed = current.initial_context.as_ref().ok_or("initial context has not been admitted")?;
                        if observed.identity.as_ref().is_none_or(|identity| identity.operation != operation) {
                            return Err("initial context publication has another operation identity".into());
                        }
                        if let Some(journal) = &journal {
                            if !journal.initial_context_publication_matches(current.context.actor, &operation, &publication.conversation)
                                .await.map_err(|error| error.to_string())? {
                                return Err("initial context publication has another conversation association".into());
                            }
                        }
                        let event = InitialContextEvent::Publish { publication };
                        let owner = observed.identity.as_ref().unwrap().actor;
                        let observation = if let Some(journal) = journal {
                            journal.record_initial_context(owner, event).await.map_err(|error| error.to_string())?
                        } else {
                            let mut next = current.initial_context;
                            crate::initial_context::apply_event(&mut next, event).map_err(|error| error.to_string())?;
                            next.unwrap()
                        };
                        Ok(InitialTransition { observation, admitted: true })
                    }.await;
                    Completion::Initial(result)
                }))
            }
            Request::PublishOwner { predecessor, reply } => {
                let owner = match &current.public_owner {
                    ActorPublicOwnerPlane::DurablePending(owner) => owner.clone(),
                    ActorPublicOwnerPlane::DurableReady(_) => { let _ = reply.send(Ok(PublicManifestCommit::Durable)); continue; }
                    _ => { let _ = reply.send(Err("public operation requires its durable owner".into())); continue; }
                };
                if let Some(intent) = &intent {
                    if intent.manifest.as_ref().map(|manifest| &manifest.public_owner.owner)
                        != predecessor.as_ref().map(|(owner, _)| owner) {
                        let _ = reply.send(Err("root public operation differs from original startup manifest".into())); continue;
                    }
                }
                (Pending::Owner(reply), Box::pin(async move {
                    Completion::Owner(match predecessor {
                        Some((predecessor, authority)) => publisher.transfer(current.context, predecessor, owner, authority).await,
                        None => publisher.initialize(current.context, owner).await,
                    })
                }))
            }
            Request::ReleaseStartup { intent: requested, reply } => {
                if intent.as_ref() != Some(&requested) || current.public_owner.ready().is_none() {
                    let _ = reply.send(Err("root release differs from original durable owner/intent".into())); continue;
                }
                (Pending::Release(reply), Box::pin(async move {
                    let result = async {
                        let journal = journal.ok_or("root startup journal is absent")?;
                        let records = journal.validated_records().await.map_err(|error| error.to_string())?;
                        let record = records.iter().find(|record| record.admission.actor == current.context.actor)
                            .ok_or("root startup durable admission is absent")?;
                        if record.terminal.is_some() || record.startup.as_ref() != Some(&requested)
                            || record.application.as_ref().and_then(|application| application.conversation.as_ref())
                                != Some(&requested.conversation) {
                            return Err("root startup requires its exact durable ApplicationBound".into());
                        }
                        Ok(())
                    }.await;
                    Completion::Release(result)
                }))
            }
            Request::ActivateStartup { .. } => unreachable!(),
        };
        state.resident.as_mut().unwrap().pending = Some((sequence, pending));
        spawn(state, address, sequence, work);
        return;
    }
}

fn spawn(state: &mut LifecycleState, address: &RactorRef<LifecycleCommand>, sequence: u64,
    work: BoxFuture<'static, Completion>) {
    let address = address.clone();
    state.tasks.insert(sequence, tokio::spawn(async move {
        let outcome = work.await;
        let _ = address.cast(LifecycleCommand::Resident(Box::new(ResidentCommand::Completed { sequence, outcome })));
    }));
}

impl<H, O> PublicOwnerPublisher for crate::ResidentActorRunner<H, O>
where H: tidepool_effect::dispatch::DispatchEffect<O> + Send + 'static,
    O: tidepool_runtime::session::OutputSink + Sync + 'static {
    fn initialize(&self, context: ActorSessionContext, owner: RecoveryPublicOwner)
        -> BoxFuture<'static, Result<DurablePublicOwnerPublication, String>> {
        let runner = self.clone();
        Box::pin(async move { runner.initialize_durable_public_owner(context, owner).await.map_err(|error| error.to_string()) })
    }
    fn transfer(&self, context: ActorSessionContext, predecessor: RecoveryPublicOwner,
        owner: RecoveryPublicOwner, authority: Arc<dyn tidepool_runtime::session::RecoverySuccessorAuthority>)
        -> BoxFuture<'static, Result<DurablePublicOwnerPublication, String>> {
        let runner = self.clone();
        Box::pin(async move { runner.transfer_recovered_root_public_owner(context, &predecessor, owner, authority)
            .await.map_err(|error| error.to_string()) })
    }
}
