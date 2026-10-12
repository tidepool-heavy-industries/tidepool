//! Startup and public-owner transitions owned by the incarnation coordinator.

use super::{LifecycleCommand, LifecycleHandle, LifecycleState};
use crate::initial_context::InitialContextEvent;
use crate::resident_actor::{ActorPublicOwnerPlane, WorkbenchPublicOwner};
use crate::{ActorDescriptor, ActorSessionContext, InitialContextObservation, RootStartupIntent};
use futures_util::future::BoxFuture;
use futures_util::FutureExt;
use ractor::{ActorRef as RactorRef, RpcReplyPort};
use std::collections::VecDeque;
use std::sync::Arc;
use tidepool_runtime::session::{
    DurablePublicOwnerPublication, PublicManifestCommit, RecoveryPublicOwner,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RootStartupState {
    Pending,
    Released,
    Activated,
}

/// Only the named public-surface operations cross into resident machine work.
/// This service contains the runner, never the Forest or directory.
pub(crate) trait PublicOwnerPublisher: Send + Sync {
    fn initialize(
        &self,
        context: ActorSessionContext,
        owner: RecoveryPublicOwner,
    ) -> BoxFuture<'static, Result<DurablePublicOwnerPublication, String>>;
    fn transfer(
        &self,
        context: ActorSessionContext,
        predecessor: RecoveryPublicOwner,
        owner: RecoveryPublicOwner,
        authority: Arc<dyn tidepool_runtime::session::RecoverySuccessorAuthority>,
    ) -> BoxFuture<'static, Result<DurablePublicOwnerPublication, String>>;
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
    pub settlement: ResidentSettlement,
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
    successor: Option<SuccessorOperation>,
}

impl ResidentState {
    pub(super) fn has_pending(&self) -> bool {
        self.pending.is_some()
            || !self.queued.is_empty()
            || self.successor.is_some()
            || !self.current.settlement.is_confirmed()
            || (self.retirement.is_some() && !self.retirement_recorded)
    }
}

pub(super) fn retirement_confirmed(state: &ResidentState) -> bool {
    state.retirement_recorded
        && state.retirement.is_some()
        && state.current.settlement.is_confirmed()
}

pub(super) fn snapshot(state: &ResidentState) -> ResidentSnapshot {
    state.current.clone()
}

#[derive(Clone)]
pub(crate) enum ResidentSettlement {
    Confirmed,
    NeedsAttention(String),
}
impl ResidentSettlement {
    pub(crate) fn is_confirmed(&self) -> bool {
        matches!(self, Self::Confirmed)
    }
}
pub(super) fn settlement_refusal(state: &ResidentState) -> Option<&str> {
    match &state.current.settlement {
        ResidentSettlement::Confirmed => None,
        ResidentSettlement::NeedsAttention(detail) => Some(detail),
    }
}

pub(super) fn native_readiness(
    state: &ResidentState,
) -> Result<Option<Arc<tidepool_runtime::session::RuntimeDurablePublicReadiness>>, String> {
    if state.successor.is_some() {
        return Err("exact successor binding is still pending".into());
    }
    match &state.current.public_owner {
        ActorPublicOwnerPlane::Ephemeral(_) => Ok(None),
        ActorPublicOwnerPlane::DurablePending(_) => {
            Err("durable actor public owner is pending".into())
        }
        ActorPublicOwnerPlane::DurableCommitted(owner) => owner
            .native_readiness()
            .map(Some)
            .ok_or_else(|| "durable public owner has no native readiness".into()),
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
    Completed {
        sequence: u64,
        outcome: Completion,
    },
    Retire(crate::ActorTerminal),
    BeginBinding {
        origin: BindingOrigin,
        reply: RpcReplyPort<Result<RootBindingOperationClaim, String>>,
    },
    RefuseSuccessor {
        sequence: u64,
        reply: RpcReplyPort<Result<(), String>>,
    },
    AbandonSuccessor(u64),
    BindSuccessor {
        claim: RootBindingOperationClaim,
        installed: Arc<tidepool_runtime::session::RuntimeDurablePublicReadiness>,
        adapter: Arc<dyn RootBindingAdapter>,
        reply: RpcReplyPort<Result<(), String>>,
    },
    BindingCompleted {
        sequence: u64,
        claim: RootBindingOperationClaim,
        result: Result<crate::JournalApplicationBindingAck, String>,
        reply: RpcReplyPort<Result<(), String>>,
    },
}

pub(super) enum Request {
    ObserveOwner {
        expected: Option<Arc<WorkbenchPublicOwner>>,
        provider: bool,
        reply: RpcReplyPort<Result<Arc<WorkbenchPublicOwner>, String>>,
    },
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
        predecessor: Option<(
            RecoveryPublicOwner,
            Arc<dyn tidepool_runtime::session::RecoverySuccessorAuthority>,
        )>,
        reply: RpcReplyPort<Result<PublicManifestCommit, String>>,
    },
    ReleaseStartup {
        intent: RootStartupIntent,
        reply: RpcReplyPort<Result<(), String>>,
    },
    ActivateStartup {
        reply: RpcReplyPort<Result<bool, String>>,
    },
}

pub(crate) struct InitialTransition {
    pub observation: InitialContextObservation,
    pub admitted: bool,
    pub execution: Option<InitialExecution>,
}

pub(crate) struct InitialExecution {
    pub answer: tokio::sync::oneshot::Receiver<crate::KernelWorkbenchReply>,
    pub control: Arc<crate::WorkbenchExecutionControl>,
}

enum Pending {
    OwnerObservation {
        owner: Arc<WorkbenchPublicOwner>,
        reply: RpcReplyPort<Result<Arc<WorkbenchPublicOwner>, String>>,
    },
    Initial(RpcReplyPort<Result<InitialTransition, String>>),
    Owner(RpcReplyPort<Result<PublicManifestCommit, String>>),
    Release(RpcReplyPort<Result<(), String>>),
    Retirement,
}

pub(super) enum Completion {
    OwnerObservation(Result<(), String>),
    Initial(Result<InitialTransition, String>),
    InitialRefused(String),
    Owner(Result<DurablePublicOwnerPublication, String>),
    Release(Result<(), String>),
    Retirement(Result<(), String>),
}

impl LifecycleHandle {
    pub(crate) async fn public_owner(
        &self,
        expected: Option<Arc<WorkbenchPublicOwner>>,
        provider: bool,
    ) -> Result<Arc<WorkbenchPublicOwner>, String> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.send_resident(Request::ObserveOwner {
            expected,
            provider,
            reply: reply.into(),
        })?;
        answer
            .await
            .map_err(|_| "public owner ended before observation".to_owned())?
    }

    pub(crate) async fn configure_resident(
        &self,
        context: ActorSessionContext,
        descriptor: ActorDescriptor,
        journal: Option<Arc<crate::ActorRecoveryJournal>>,
        publisher: Arc<dyn PublicOwnerPublisher>,
        startup: Option<RootStartupIntent>,
        fresh_context_seed: Option<String>,
    ) -> Result<(), String> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.address
            .cast(LifecycleCommand::Resident(Box::new(
                ResidentCommand::Configure {
                    context,
                    descriptor,
                    journal,
                    publisher,
                    startup,
                    fresh_context_seed,
                    reply: reply.into(),
                },
            )))
            .map_err(|error| error.to_string())?;
        answer
            .await
            .map_err(|_| "resident configuration owner ended".to_owned())?
    }

    pub(crate) fn policy_installed(&self, policy: Option<crate::InstalledToolLease>) {
        let _ = self.address.cast(LifecycleCommand::Resident(Box::new(
            ResidentCommand::PolicyInstalled(policy),
        )));
    }

    pub(crate) fn retire_resident(&self, terminal: crate::ActorTerminal) {
        let _ = self.address.cast(LifecycleCommand::Resident(Box::new(
            ResidentCommand::Retire(terminal),
        )));
    }

    pub(crate) async fn begin_initial_context(
        &self,
        event: InitialContextEvent,
    ) -> Result<InitialTransition, String> {
        self.initial(event, true, None).await
    }

    pub(crate) async fn change_initial_context(
        &self,
        event: InitialContextEvent,
        expected: tidepool_runtime::session::WorkbenchExecutionId,
    ) -> Result<InitialContextObservation, String> {
        self.initial(event, false, Some(expected))
            .await
            .map(|value| value.observation)
    }

    async fn initial(
        &self,
        event: InitialContextEvent,
        begin: bool,
        expected: Option<tidepool_runtime::session::WorkbenchExecutionId>,
    ) -> Result<InitialTransition, String> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.send_resident(Request::Initial {
            event,
            begin,
            expected,
            reply: reply.into(),
        })?;
        answer
            .await
            .map_err(|_| "initial context owner ended before settlement".to_owned())?
    }

    pub(crate) async fn acknowledge_initial_context(
        &self,
        operation: tidepool_runtime::session::WorkbenchExecutionId,
        publication: crate::InitialContextPublication,
    ) -> Result<InitialContextObservation, String> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.send_resident(Request::AcknowledgeInitial {
            operation,
            publication,
            reply: reply.into(),
        })?;
        answer
            .await
            .map_err(|_| "initial context owner ended before acknowledgment".to_owned())?
            .map(|value| value.observation)
    }

    pub(crate) async fn publish_public_owner(
        &self,
        predecessor: Option<(
            RecoveryPublicOwner,
            Arc<dyn tidepool_runtime::session::RecoverySuccessorAuthority>,
        )>,
    ) -> Result<PublicManifestCommit, String> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.send_resident(Request::PublishOwner {
            predecessor,
            reply: reply.into(),
        })?;
        answer
            .await
            .map_err(|_| "public owner ended before publication settlement".to_owned())?
    }

    pub(crate) async fn release_startup(&self, intent: RootStartupIntent) -> Result<(), String> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.send_resident(Request::ReleaseStartup {
            intent,
            reply: reply.into(),
        })?;
        answer
            .await
            .map_err(|_| "root startup owner ended before release".to_owned())?
    }

    pub(crate) async fn activate_startup(&self) -> Result<bool, String> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.send_resident(Request::ActivateStartup {
            reply: reply.into(),
        })?;
        answer
            .await
            .map_err(|_| "root startup owner ended before activation".to_owned())?
    }

    fn send_resident(&self, request: Request) -> Result<(), String> {
        self.address
            .cast(LifecycleCommand::Resident(Box::new(
                ResidentCommand::Request(request),
            )))
            .map_err(|error| error.to_string())
    }
}

pub(super) fn handle(
    state: &mut LifecycleState,
    address: &RactorRef<LifecycleCommand>,
    command: ResidentCommand,
) {
    match command {
        ResidentCommand::Configure {
            context,
            descriptor,
            journal,
            publisher,
            startup,
            fresh_context_seed,
            reply,
        } => {
            let result = if state.resident.is_some()
                || state.is_closed()
                || context.actor != state.identity
                || context.placement != descriptor.placement()
            {
                Err("resident configuration differs from its original incarnation".to_owned())
            } else {
                let owner = match descriptor.persistence_policy() {
                    crate::ActorPersistencePolicy::Ephemeral => {
                        WorkbenchPublicOwner::issue(&context, &descriptor, None)
                            .map(ActorPublicOwnerPlane::Ephemeral)
                            .map_err(|error| error.to_string())
                    }
                    crate::ActorPersistencePolicy::Durable => descriptor
                        .actor_path()
                        .and_then(|path| {
                            RecoveryPublicOwner::new(path, context.actor.incarnation.0)
                        })
                        .map(ActorPublicOwnerPlane::DurablePending)
                        .ok_or_else(|| "durable actor requires its canonical admitted path".into()),
                };
                owner.map(|public_owner| {
                    state.resident = Some(ResidentState {
                        current: ResidentSnapshot {
                            context,
                            descriptor,
                            public_owner,
                            initial_context: None,
                            startup: startup.as_ref().map(|_| RootStartupState::Pending),
                            installed_policy: None,
                            fresh_context_seed,
                            settlement: ResidentSettlement::Confirmed,
                        },
                        journal,
                        publisher,
                        startup_intent: startup,
                        pending: None,
                        queued: VecDeque::new(),
                        retirement: None,
                        retirement_recorded: false,
                        successor: None,
                    });
                })
            };
            state.publish();
            let _ = reply.send(result);
        }
        ResidentCommand::PolicyInstalled(policy) => {
            if !state.is_closed() {
                if let Some(resident) = &mut state.resident {
                    resident.current.installed_policy = policy;
                }
            }
        }
        ResidentCommand::Request(request) => {
            let closed = state.is_closed();
            if let Some(resident) = &mut state.resident {
                let admitted_completion = match &request {
                    Request::Initial {
                        begin: false,
                        expected: Some(expected),
                        event,
                        ..
                    } => {
                        matches!(
                            event,
                            InitialContextEvent::Retain { .. }
                                | InitialContextEvent::Fail { .. }
                                | InitialContextEvent::Uncertain { .. }
                        ) && resident
                            .current
                            .initial_context
                            .as_ref()
                            .is_some_and(|current| {
                                matches!(current.state, crate::InitialContextState::Preparing)
                                    && current
                                        .identity
                                        .as_ref()
                                        .is_some_and(|identity| &identity.operation == expected)
                            })
                    }
                    _ => false,
                };
                if (!closed && resident.current.settlement.is_confirmed()) || admitted_completion {
                    resident.queued.push_back(request);
                } else {
                    refuse(request, "actor lifecycle admission is closed");
                }
            } else {
                refuse(request, "resident lifecycle is not configured");
            }
        }
        ResidentCommand::Retire(terminal) => {
            if let Some(resident) = &mut state.resident {
                resident.retirement = Some(terminal);
            }
        }
        ResidentCommand::Completed { sequence, outcome } => {
            complete(state, address, sequence, outcome)
        }
        ResidentCommand::BeginBinding { origin, reply } => {
            let result = if state.is_closed() {
                Err("successor lifecycle is closing".into())
            } else if state
                .resident
                .as_ref()
                .is_none_or(|resident| resident.successor.is_some())
            {
                Err("successor operation is unavailable or already owned".into())
            } else {
                let sequence = state.next_operation();
                let resident = state.resident.as_mut().unwrap();
                let owner = match &resident.current.public_owner {
                    ActorPublicOwnerPlane::DurablePending(owner) => Some(owner.clone()),
                    ActorPublicOwnerPlane::DurableCommitted(owner) => owner.durable().cloned(),
                    _ => None,
                };
                let original = resident
                    .startup_intent
                    .as_ref()
                    .and_then(|intent| intent.manifest.as_ref())
                    .map(|manifest| &manifest.public_owner.owner);
                let origin_matches = match &origin {
                    BindingOrigin::Fresh => original.is_none(),
                    BindingOrigin::Successor(owner) => {
                        original.is_none_or(|original| original == owner)
                    }
                };
                if !origin_matches {
                    let _ = reply.send(Err(
                        "binding origin differs from original startup manifest".into(),
                    ));
                    return;
                }
                let journal = resident.journal.clone();
                owner
                    .zip(journal)
                    .ok_or_else(|| {
                        "successor operation requires its exact durable owner and journal"
                            .to_owned()
                    })
                    .map(|(owner, journal)| {
                        resident.successor = Some(SuccessorOperation {
                            sequence,
                            origin: origin.clone(),
                            owner: owner.clone(),
                            stage: SuccessorStage::Reserved,
                        });
                        RootBindingOperationClaim {
                            sequence,
                            actor: state.identity,
                            placement: resident.current.context.placement,
                            owner,
                            origin,
                            lifecycle: address.clone(),
                            journal,
                            armed: true,
                        }
                    })
            };
            if result.is_ok() && state.phase == super::LifecyclePhase::Open {
                state.phase = super::LifecyclePhase::Installing;
            }
            state.publish();
            let _ = reply.send(result);
        }
        ResidentCommand::RefuseSuccessor { sequence, reply } => {
            let resident = state.resident.as_mut();
            let result = match resident {
                Some(resident)
                    if resident.successor.as_ref().is_some_and(|operation| {
                        operation.sequence == sequence
                            && matches!(operation.stage, SuccessorStage::Reserved)
                    }) =>
                {
                    resident.successor = None;
                    Ok(())
                }
                _ => Err("successor refusal cannot prove absence of an admitted effect".into()),
            };
            if result.is_ok() && state.phase == super::LifecyclePhase::Installing {
                state.phase = super::LifecyclePhase::Open;
            }
            state.publish();
            let _ = reply.send(result);
        }
        ResidentCommand::AbandonSuccessor(sequence) => {
            if let Some(resident) = state.resident.as_mut() {
                if let Some(operation) = resident
                    .successor
                    .as_mut()
                    .filter(|operation| operation.sequence == sequence)
                {
                    operation.stage = SuccessorStage::NeedsAttention;
                    resident.current.settlement = ResidentSettlement::NeedsAttention(
                        "successor operation was abandoned".into(),
                    );
                }
            }
        }
        ResidentCommand::BindSuccessor {
            claim,
            installed,
            adapter,
            reply,
        } => {
            let matches = state.resident.as_ref().is_some_and(|resident| {
                resident.successor.as_ref().is_some_and(|operation| {
                    operation.sequence == claim.sequence
                        && !matches!(
                            operation.stage,
                            SuccessorStage::Binding | SuccessorStage::NeedsAttention
                        )
                }) && claim.actor == state.identity
                    && claim.placement == resident.current.context.placement
                    && installed.durable_owner() == &claim.owner
                    && installed.session() == claim.placement.session
                    && installed.scope() == claim.placement.lexical_scope
            });
            if !matches {
                let _ = reply.send(Err(
                    "successor binding differs from its admitted operation".into()
                ));
            } else {
                let sequence = claim.sequence;
                state
                    .resident
                    .as_mut()
                    .unwrap()
                    .successor
                    .as_mut()
                    .unwrap()
                    .stage = SuccessorStage::Binding;
                let address = address.clone();
                state.tasks.insert(sequence, tokio::spawn(async move {
                    let result = std::panic::AssertUnwindSafe(adapter.bind(&claim)).catch_unwind().await
                        .unwrap_or_else(|_| Err("successor binding adapter panicked before confirmed settlement".into()));
                    let _ = address.cast(LifecycleCommand::Resident(Box::new(ResidentCommand::BindingCompleted {
                        sequence, claim, result, reply,
                    })));
                }));
            }
        }
        ResidentCommand::BindingCompleted {
            sequence,
            mut claim,
            result,
            reply,
        } => {
            let matches = state.resident.as_ref().is_some_and(|resident| {
                resident.successor.as_ref().is_some_and(|operation| {
                    operation.sequence == sequence
                        && matches!(operation.stage, SuccessorStage::Binding)
                }) && claim.sequence == sequence
                    && claim.actor == state.identity
            });
            if !matches {
                let _ = reply.send(Err(
                    "successor binding completion has stale correlation".into()
                ));
            } else {
                state.tasks.remove(&sequence);
                let resident = state.resident.as_mut().unwrap();
                let result = result.and_then(|ack| {
                    if ack.actor() != claim.actor
                        || resident
                            .journal
                            .as_ref()
                            .is_none_or(|journal| !ack.belongs_to(journal))
                        || resident
                            .startup_intent
                            .as_ref()
                            .is_some_and(|intent| ack.conversation() != &intent.conversation)
                    {
                        Err("binding acknowledgement differs from the issuing owner/intent".into())
                    } else {
                        Ok(())
                    }
                });
                if result.is_ok() {
                    resident.successor = None;
                } else {
                    resident.successor.as_mut().unwrap().stage = SuccessorStage::NeedsAttention;
                    resident.current.settlement = ResidentSettlement::NeedsAttention(
                        "successor binding settlement is unconfirmed".into(),
                    );
                }
                claim.armed = false;
                if result.is_ok() && state.phase == super::LifecyclePhase::Installing {
                    state.phase = super::LifecyclePhase::Open;
                }
                state.publish();
                let _ = reply.send(result);
            }
        }
    }
    advance(state, address);
}

fn refuse(request: Request, detail: &str) {
    match request {
        Request::ObserveOwner { reply, .. } => {
            let _ = reply.send(Err(detail.into()));
        }
        Request::Initial { reply, .. } | Request::AcknowledgeInitial { reply, .. } => {
            let _ = reply.send(Err(detail.into()));
        }
        Request::PublishOwner { reply, .. } => {
            let _ = reply.send(Err(detail.into()));
        }
        Request::ReleaseStartup { reply, .. } => {
            let _ = reply.send(Err(detail.into()));
        }
        Request::ActivateStartup { reply } => {
            let _ = reply.send(Err(detail.into()));
        }
    }
}

fn complete(
    state: &mut LifecycleState,
    address: &RactorRef<LifecycleCommand>,
    sequence: u64,
    outcome: Completion,
) {
    let Some(resident) = &mut state.resident else {
        return;
    };
    if resident
        .pending
        .as_ref()
        .is_none_or(|(issued, _)| *issued != sequence)
    {
        return;
    }
    let stage_matches = matches!(
        (&resident.pending.as_ref().unwrap().1, &outcome),
        (
            Pending::OwnerObservation { .. },
            Completion::OwnerObservation(_)
        ) | (
            Pending::Initial(_),
            Completion::Initial(_) | Completion::InitialRefused(_)
        ) | (Pending::Owner(_), Completion::Owner(_))
            | (Pending::Release(_), Completion::Release(_))
            | (Pending::Retirement, Completion::Retirement(_))
    );
    if !stage_matches {
        return;
    }
    let (_, pending) = resident.pending.take().expect("matched resident operation");
    state.tasks.remove(&sequence);
    match (pending, outcome) {
        (Pending::OwnerObservation { owner, reply }, Completion::OwnerObservation(result)) => {
            let result = if state.is_closed() {
                Err("public-owner observation closed during native validation".into())
            } else {
                result.map(|()| owner)
            };
            state.publish();
            let _ = reply.send(result);
        }
        (Pending::Initial(reply), Completion::InitialRefused(error)) => {
            // This exact correlated operation established refusal before mutation.
            // It cannot turn known nonpublication into an uncertain durable effect.
            let _ = reply.send(Err(error));
        }
        (Pending::Initial(reply), Completion::Initial(result)) => {
            if let Err(error) = &result {
                resident.current.settlement = ResidentSettlement::NeedsAttention(error.clone());
            }
            let mut result = result.map(|transition| {
                resident.current.initial_context = Some(transition.observation.clone());
                transition
            });
            let start = result
                .as_ref()
                .ok()
                .filter(|transition| {
                    transition.admitted
                        && matches!(
                            transition.observation.state,
                            crate::InitialContextState::Preparing
                        )
                })
                .and_then(|transition| transition.observation.identity.clone());
            if let Some(identity) = start {
                let resident = state.resident.as_ref().unwrap();
                let request = tidepool_runtime::session::WorkbenchRequest::for_retained_operation(
                    crate::initial_context::INITIAL_CONTEXT_SLOT,
                    serde_json::json!({"operation":identity.operation.as_str(),"callerSeed":resident.current.initial_context.as_ref().unwrap().caller_seed}))
                    .with_execution_id(identity.operation.clone());
                let mut invocation = crate::ActorWorkbenchInvocation::issued(
                    request,
                    resident.current.installed_policy.clone(),
                    None,
                );
                invocation.initial_context = true;
                let control = crate::WorkbenchExecutionControl::untracked();
                let (answer, receiver) = tokio::sync::oneshot::channel();
                let enqueue = state.enqueue(
                    address,
                    crate::KernelMessage::Workbench {
                        invocation,
                        control: Some(control.clone()),
                        reply: answer.into(),
                    },
                    false,
                );
                match enqueue {
                    Ok(()) => {
                        result.as_mut().unwrap().execution = Some(InitialExecution {
                            answer: receiver,
                            control,
                        })
                    }
                    Err(error) => {
                        let (ignore, _) = tokio::sync::oneshot::channel();
                        state
                            .resident
                            .as_mut()
                            .unwrap()
                            .queued
                            .push_back(Request::Initial {
                                event: InitialContextEvent::Fail {
                                    detail: error.to_string(),
                                    cancelled: true,
                                    receipts: serde_json::json!([]),
                                },
                                begin: false,
                                expected: Some(identity.operation),
                                reply: ignore.into(),
                            });
                        result = Err(error.to_string());
                    }
                }
            }
            state.publish();
            // If this observer disappeared, cancel only admitted execution;
            // the executor's finalization still retains its eventual outcome.
            if let Err(ractor::MessagingErr::SendErr(Ok(transition))) = reply.send(result) {
                if let Some(execution) = transition.execution {
                    execution.control.request_cancellation();
                }
            }
        }
        (Pending::Owner(reply), Completion::Owner(result)) => {
            if let Err(error) = &result {
                resident.current.settlement = ResidentSettlement::NeedsAttention(error.clone());
            }
            let result = result.and_then(|publication| match publication {
                DurablePublicOwnerPublication::Committed(readiness) => {
                    let owner = WorkbenchPublicOwner::issue(
                        &resident.current.context,
                        &resident.current.descriptor,
                        Some(readiness),
                    )
                    .map_err(|error| error.to_string())?;
                    resident.current.public_owner = ActorPublicOwnerPlane::DurableCommitted(owner);
                    Ok(PublicManifestCommit::Durable)
                }
                DurablePublicOwnerPublication::Refused { error } => {
                    Ok(PublicManifestCommit::Refused { error })
                }
                DurablePublicOwnerPublication::RecoveryRequired { error } => {
                    Ok(PublicManifestCommit::RecoveryRequired { error })
                }
            });
            state.publish();
            let _ = reply.send(result);
        }
        (Pending::Release(reply), Completion::Release(result)) => {
            if result.is_ok() {
                resident.current.startup = Some(RootStartupState::Released);
            }
            state.publish();
            let _ = reply.send(result);
        }
        (Pending::Retirement, Completion::Retirement(result)) => match result {
            Ok(()) => resident.retirement_recorded = true,
            Err(error) => resident.current.settlement = ResidentSettlement::NeedsAttention(error),
        },
        _ => unreachable!("resident completion kind differs from its issued pending operation"),
    }
}

fn advance(state: &mut LifecycleState, address: &RactorRef<LifecycleCommand>) {
    loop {
        let closed = state.is_closed();
        let Some(resident) = state.resident.as_mut() else {
            return;
        };
        if resident.pending.is_some() || !resident.current.settlement.is_confirmed() {
            return;
        }
        let Some(request) = resident.queued.pop_front() else {
            if !resident.retirement_recorded && resident.successor.is_none() {
                if let Some(terminal) = resident.retirement.clone() {
                    let journal = resident.journal.clone();
                    let actor = state.identity;
                    let sequence = state.next_operation();
                    state.resident.as_mut().unwrap().pending =
                        Some((sequence, Pending::Retirement));
                    spawn(
                        state,
                        address,
                        sequence,
                        Box::pin(async move {
                            Completion::Retirement(match journal {
                                Some(journal) => journal
                                    .retire(actor, terminal)
                                    .await
                                    .map_err(|error| error.to_string()),
                                None => Ok(()),
                            })
                        }),
                    );
                }
            }
            return;
        };
        if let Request::ActivateStartup { reply } = request {
            let result = match resident.current.startup {
                Some(RootStartupState::Released) if !closed => {
                    resident.current.startup = Some(RootStartupState::Activated);
                    Ok(true)
                }
                Some(RootStartupState::Activated) => Ok(false),
                _ => Err("root startup has not been durably released".into()),
            };
            state.publish();
            let _ = reply.send(result);
            continue;
        }
        if closed
            && matches!(
                &request,
                Request::PublishOwner { .. } | Request::ReleaseStartup { .. }
            )
        {
            refuse(request, "actor closed before external publication started");
            continue;
        }
        let current = resident.current.clone();
        let journal = resident.journal.clone();
        let publisher = Arc::clone(&resident.publisher);
        let intent = resident.startup_intent.clone();
        let sequence = state.next_operation();
        let (pending, work): (Pending, BoxFuture<'static, Completion>) = match request {
            Request::ObserveOwner {
                expected,
                provider,
                reply,
            } => {
                let owner = current.public_owner.ready().cloned();
                if closed
                    || owner.as_ref().is_none_or(|owner| {
                        expected
                            .as_ref()
                            .is_some_and(|expected| !Arc::ptr_eq(owner, expected))
                    })
                    || (provider
                        && (state.resident.as_ref().unwrap().successor.is_some()
                            || current
                                .startup
                                .is_some_and(|startup| startup != RootStartupState::Activated)))
                {
                    let _ = reply.send(Err(
                        "public owner does not match its live exact admission".into()
                    ));
                    continue;
                }
                let owner = owner.unwrap();
                let native = owner.native_readiness();
                (
                    Pending::OwnerObservation { owner, reply },
                    Box::pin(async move {
                        Completion::OwnerObservation(match native {
                            Some(native) => native
                                .check_ready_async()
                                .await
                                .map_err(|error| error.to_string()),
                            None => Ok(()),
                        })
                    }),
                )
            }
            Request::Initial {
                event,
                begin,
                expected,
                reply,
            } => {
                if begin {
                    if let Some(observation) = current.initial_context {
                        let _ = reply.send(Ok(InitialTransition {
                            observation,
                            admitted: false,
                            execution: None,
                        }));
                        continue;
                    }
                }
                let owner = current
                    .initial_context
                    .as_ref()
                    .and_then(|value| value.identity.as_ref())
                    .map_or(current.context.actor, |identity| identity.actor);
                (
                    Pending::Initial(reply),
                    Box::pin(async move {
                        let result = async {
                            if begin {
                                if let Some(journal) = &journal {
                                    if let Some(observation) = journal
                                        .initial_context(owner)
                                        .await
                                        .map_err(|error| error.to_string())?
                                    {
                                        return Ok(InitialTransition {
                                            observation,
                                            admitted: false,
                                            execution: None,
                                        });
                                    }
                                }
                            } else if current
                                .initial_context
                                .as_ref()
                                .and_then(|value| value.identity.as_ref())
                                .is_none_or(|identity| {
                                    Some(&identity.operation) != expected.as_ref()
                                })
                            {
                                return Err(
                                    "initial context completion has another operation identity"
                                        .into(),
                                );
                            }
                            let observation = if let Some(journal) = journal {
                                journal
                                    .record_initial_context(owner, event)
                                    .await
                                    .map_err(|error| error.to_string())?
                            } else {
                                let mut next = current.initial_context;
                                crate::initial_context::apply_event(&mut next, event)
                                    .map_err(|error| error.to_string())?;
                                next.ok_or_else(|| {
                                    "initial context transition produced no result".to_owned()
                                })?
                            };
                            Ok(InitialTransition {
                                observation,
                                admitted: true,
                                execution: None,
                            })
                        }
                        .await;
                        Completion::Initial(result)
                    }),
                )
            }
            Request::AcknowledgeInitial {
                operation,
                publication,
                reply,
            } => {
                let Some(observed) = current.initial_context.as_ref() else {
                    let _ = reply.send(Err("initial context has not been admitted".into()));
                    continue;
                };
                let Some(identity) = observed
                    .identity
                    .as_ref()
                    .filter(|identity| identity.operation == operation)
                else {
                    let _ = reply.send(Err(
                        "initial context publication has another operation identity".into(),
                    ));
                    continue;
                };
                let owner = identity.actor;
                (
                    Pending::Initial(reply),
                    Box::pin(async move {
                        if let Some(journal) = &journal {
                            match journal.initial_context_publication_matches(current.context.actor, &operation, &publication.conversation).await {
                                Ok(true) => {},
                                Ok(false) => return Completion::InitialRefused("initial context publication has another conversation association".into()),
                                Err(error) => return Completion::Initial(Err(error.to_string())),
                            }
                        }
                        let result = async {
                            let event = InitialContextEvent::Publish { publication };
                            let observation = if let Some(journal) = journal {
                                journal
                                    .record_initial_context(owner, event)
                                    .await
                                    .map_err(|error| error.to_string())?
                            } else {
                                let mut next = current.initial_context;
                                crate::initial_context::apply_event(&mut next, event)
                                    .map_err(|error| error.to_string())?;
                                next.unwrap()
                            };
                            Ok(InitialTransition {
                                observation,
                                admitted: true,
                                execution: None,
                            })
                        }
                        .await;
                        Completion::Initial(result)
                    }),
                )
            }
            Request::PublishOwner { predecessor, reply } => {
                if let Some((predecessor, _)) = &predecessor {
                    if state
                        .resident
                        .as_ref()
                        .unwrap()
                        .successor
                        .as_ref()
                        .is_none_or(|claim| !matches!(&claim.origin, BindingOrigin::Successor(owner) if owner == predecessor))
                    {
                        let _ = reply.send(Err(
                            "public transfer requires its issued successor operation claim".into(),
                        ));
                        continue;
                    }
                }
                let owner = match &current.public_owner {
                    ActorPublicOwnerPlane::DurablePending(owner) => owner.clone(),
                    ActorPublicOwnerPlane::DurableCommitted(_) => {
                        let _ = reply.send(Ok(PublicManifestCommit::Durable));
                        continue;
                    }
                    _ => {
                        let _ =
                            reply.send(Err("public operation requires its durable owner".into()));
                        continue;
                    }
                };
                if let Some(intent) = &intent {
                    if intent
                        .manifest
                        .as_ref()
                        .map(|manifest| &manifest.public_owner.owner)
                        != predecessor.as_ref().map(|(owner, _)| owner)
                    {
                        let _ = reply.send(Err(
                            "root public operation differs from original startup manifest".into(),
                        ));
                        continue;
                    }
                }
                if let Some(successor) = state.resident.as_mut().unwrap().successor.as_mut() {
                    successor.stage = SuccessorStage::Transferring;
                }
                (
                    Pending::Owner(reply),
                    Box::pin(async move {
                        Completion::Owner(match predecessor {
                            Some((predecessor, authority)) => {
                                publisher
                                    .transfer(current.context, predecessor, owner, authority)
                                    .await
                            }
                            None => publisher.initialize(current.context, owner).await,
                        })
                    }),
                )
            }
            Request::ReleaseStartup {
                intent: requested,
                reply,
            } => {
                if intent.as_ref() != Some(&requested) || current.public_owner.ready().is_none() {
                    let _ = reply.send(Err(
                        "root release differs from original durable owner/intent".into(),
                    ));
                    continue;
                }
                (
                    Pending::Release(reply),
                    Box::pin(async move {
                        let result = async {
                            let journal = journal.ok_or("root startup journal is absent")?;
                            let records = journal
                                .validated_records()
                                .await
                                .map_err(|error| error.to_string())?;
                            let record = records
                                .iter()
                                .find(|record| record.admission.actor == current.context.actor)
                                .ok_or("root startup durable admission is absent")?;
                            if record.terminal.is_some()
                                || record.startup.as_ref() != Some(&requested)
                                || record
                                    .application
                                    .as_ref()
                                    .and_then(|application| application.conversation.as_ref())
                                    != Some(&requested.conversation)
                            {
                                return Err(
                                    "root startup requires its exact durable ApplicationBound"
                                        .into(),
                                );
                            }
                            Ok(())
                        }
                        .await;
                        Completion::Release(result)
                    }),
                )
            }
            Request::ActivateStartup { .. } => unreachable!(),
        };
        state.resident.as_mut().unwrap().pending = Some((sequence, pending));
        spawn(state, address, sequence, work);
        return;
    }
}

fn spawn(
    state: &mut LifecycleState,
    address: &RactorRef<LifecycleCommand>,
    sequence: u64,
    work: BoxFuture<'static, Completion>,
) {
    let address = address.clone();
    let kind = match &state.resident.as_ref().unwrap().pending.as_ref().unwrap().1 {
        Pending::OwnerObservation { .. } => 4,
        Pending::Initial(_) => 0,
        Pending::Owner(_) => 1,
        Pending::Release(_) => 2,
        Pending::Retirement => 3,
    };
    state.tasks.insert(
        sequence,
        tokio::spawn(async move {
            let outcome = match std::panic::AssertUnwindSafe(work).catch_unwind().await {
                Ok(outcome) => outcome,
                Err(_) => {
                    let error =
                        "resident owner task panicked before confirmed settlement".to_owned();
                    match kind {
                        0 => Completion::Initial(Err(error)),
                        1 => Completion::Owner(Err(error)),
                        2 => Completion::Release(Err(error)),
                        _ => Completion::Retirement(Err(error)),
                    }
                }
            };
            let _ = address.cast(LifecycleCommand::Resident(Box::new(
                ResidentCommand::Completed { sequence, outcome },
            )));
        }),
    );
}

impl<H, O> PublicOwnerPublisher for crate::ResidentActorRunner<H, O>
where
    H: tidepool_effect::dispatch::DispatchEffect<O> + Send + 'static,
    O: tidepool_runtime::session::OutputSink + Sync + 'static,
{
    fn initialize(
        &self,
        context: ActorSessionContext,
        owner: RecoveryPublicOwner,
    ) -> BoxFuture<'static, Result<DurablePublicOwnerPublication, String>> {
        let runner = self.clone();
        Box::pin(async move {
            runner
                .initialize_durable_public_owner(context, owner)
                .await
                .map_err(|error| error.to_string())
        })
    }
    fn transfer(
        &self,
        context: ActorSessionContext,
        predecessor: RecoveryPublicOwner,
        owner: RecoveryPublicOwner,
        authority: Arc<dyn tidepool_runtime::session::RecoverySuccessorAuthority>,
    ) -> BoxFuture<'static, Result<DurablePublicOwnerPublication, String>> {
        let runner = self.clone();
        Box::pin(async move {
            runner
                .transfer_recovered_root_public_owner(context, &predecessor, owner, authority)
                .await
                .map_err(|error| error.to_string())
        })
    }
}

#[derive(Clone)]
enum BindingOrigin {
    Fresh,
    Successor(RecoveryPublicOwner),
}

#[derive(Debug, thiserror::Error)]
pub enum RootBindingClaimError {
    #[error("a fresh binding operation has no predecessor owner")]
    Fresh,
}

struct SuccessorOperation {
    sequence: u64,
    origin: BindingOrigin,
    owner: RecoveryPublicOwner,
    stage: SuccessorStage,
}
enum SuccessorStage {
    Reserved,
    Transferring,
    Binding,
    NeedsAttention,
}

/// Affine ownership of one successor's transfer and verified binding operation.
/// Immutable getters authenticate journal facts; they do not admit other work.
pub struct RootBindingOperationClaim {
    sequence: u64,
    actor: crate::ActorRef,
    placement: crate::ActorPlacement,
    owner: RecoveryPublicOwner,
    origin: BindingOrigin,
    lifecycle: RactorRef<LifecycleCommand>,
    journal: Arc<crate::ActorRecoveryJournal>,
    armed: bool,
}

/// Trusted host adapter at the existing Store binding boundary. Production
/// implementations confirm the exact Store binding before acknowledging the
/// matching journal event. This is a trusted adapter contract, not a proof
/// against a malicious implementation.
pub trait RootBindingAdapter: Send + Sync {
    fn bind<'a>(
        &'a self,
        claim: &'a RootBindingOperationClaim,
    ) -> BoxFuture<'a, Result<crate::JournalApplicationBindingAck, String>>;
}

impl RootBindingOperationClaim {
    pub fn successor_actor(&self) -> crate::ActorRef {
        self.actor
    }
    pub fn successor_placement(&self) -> crate::ActorPlacement {
        self.placement
    }
    pub fn successor_owner(&self) -> &RecoveryPublicOwner {
        &self.owner
    }
    pub fn predecessor_owner(&self) -> Result<&RecoveryPublicOwner, RootBindingClaimError> {
        match &self.origin {
            BindingOrigin::Fresh => Err(RootBindingClaimError::Fresh),
            BindingOrigin::Successor(owner) => Ok(owner),
        }
    }
    pub fn operation(&self) -> u64 {
        self.sequence
    }
    pub fn journal_matches(&self, journal: &crate::ActorRecoveryJournal) -> bool {
        std::ptr::eq(self.journal.as_ref(), journal)
    }

    pub async fn finish_binding(
        self,
        installed: Arc<tidepool_runtime::session::RuntimeDurablePublicReadiness>,
        adapter: Arc<dyn RootBindingAdapter>,
    ) -> Result<(), String> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        let lifecycle = self.lifecycle.clone();
        // Ownership moves into the command before the caller may await/drop.
        // The command and completion retain the still-armed claim.
        lifecycle
            .cast(LifecycleCommand::Resident(Box::new(
                ResidentCommand::BindSuccessor {
                    claim: self,
                    installed,
                    adapter,
                    reply: reply.into(),
                },
            )))
            .map_err(|error| error.to_string())?;
        answer
            .await
            .map_err(|_| "successor binding owner ended before settlement".to_owned())?
    }

    pub async fn refuse(mut self) -> Result<(), String> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.lifecycle
            .cast(LifecycleCommand::Resident(Box::new(
                ResidentCommand::RefuseSuccessor {
                    sequence: self.sequence,
                    reply: reply.into(),
                },
            )))
            .map_err(|error| error.to_string())?;
        let result = answer
            .await
            .map_err(|_| "successor refusal owner ended before settlement".to_owned())?;
        if result.is_ok() {
            self.armed = false;
        }
        result
    }
}

impl Drop for RootBindingOperationClaim {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.lifecycle.cast(LifecycleCommand::Resident(Box::new(
                ResidentCommand::AbandonSuccessor(self.sequence),
            )));
        }
    }
}

impl LifecycleHandle {
    pub async fn begin_successor_operation(
        &self,
        predecessor: RecoveryPublicOwner,
    ) -> Result<RootBindingOperationClaim, String> {
        self.begin_binding_operation(BindingOrigin::Successor(predecessor))
            .await
    }

    pub async fn begin_root_binding_operation(&self) -> Result<RootBindingOperationClaim, String> {
        self.begin_binding_operation(BindingOrigin::Fresh).await
    }

    async fn begin_binding_operation(
        &self,
        origin: BindingOrigin,
    ) -> Result<RootBindingOperationClaim, String> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.address
            .cast(LifecycleCommand::Resident(Box::new(
                ResidentCommand::BeginBinding {
                    origin,
                    reply: reply.into(),
                },
            )))
            .map_err(|error| error.to_string())?;
        answer
            .await
            .map_err(|_| "successor owner ended before operation admission".to_owned())?
    }
}
