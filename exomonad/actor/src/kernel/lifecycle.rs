//! One serialized owner for an exact incarnation's ingress and lifetime.
//!
//! Executor scheduling and hosted-result custody retain their existing owners.
//! Pending native operations return by correlated messages; this actor never
//! awaits a Store or compiler operation in a message handler.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use futures_util::{future::BoxFuture, FutureExt};
use ractor::{Actor, ActorProcessingErr, ActorRef as RactorRef, RpcReplyPort};
use std::panic::AssertUnwindSafe;
use tokio::sync::watch;

use super::{HostedCellPublications, HostedCellSlot, KernelCallFailure, KernelMessage};
use crate::{ActorRef, ActorRuntimeObservationHandle, ActorTerminal, RetainedActorExit};

pub(crate) mod resident;
mod transfer;
pub(crate) use transfer::ReplacementIngressError;

#[cfg(test)]
mod control_cluster_tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecyclePhase {
    Starting,
    Open,
    Installing,
    IdleCheck,
    Closing,
    Retired,
    Finalized,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderPhase {
    Detached,
    NotStarted,
    Active,
    Idle,
    NeedsAttention,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeSettlement {
    Confirmed,
    NeedsAttention(String),
}

#[derive(Clone)]
pub struct LifecycleSnapshot {
    executor: Option<RactorRef<KernelMessage>>,
    pub owner_available: bool,
    source_forward: Option<LifecycleHandle>,
    pub native_settlement: NativeSettlement,
    pub transfer_unconfirmed: Option<String>,
    pub phase: LifecyclePhase,
    pub transactions: usize,
    pub ingress: usize,
    pub provider: ProviderPhase,
    pub(super) resident: Option<resident::ResidentSnapshot>,
}

/// An immutable observation is never permission to enqueue or start work.
#[derive(Clone)]
pub struct LifecycleHandle {
    identity: ActorRef,
    terminal: RetainedActorExit,
    address: RactorRef<LifecycleCommand>,
    observations: watch::Receiver<LifecycleSnapshot>,
    hosted_cell: HostedCellSlot,
}

/// The adapter implements this using its owning durable Store and exact binding.
/// The returned future runs outside the coordinator's serialized handler.
pub trait ProviderIdleProbe: Send + Sync {
    fn check_idle(&self) -> BoxFuture<'static, Result<bool, String>>;
}

struct NativeProvider {
    phase: ProviderPhase,
    observation: ActorRuntimeObservationHandle,
    readiness: Arc<tidepool_runtime::session::RuntimeDurablePublicReadiness>,
    probe: Arc<dyn ProviderIdleProbe>,
    turn: Option<crate::ProviderTurnLease>,
    sequence: Option<u64>,
}

pub(super) struct LifecycleState {
    pub(super) identity: ActorRef,
    terminal: RetainedActorExit,
    pub(super) phase: LifecyclePhase,
    executor: Option<RactorRef<KernelMessage>>,
    hosted_sealed: bool,
    source_forward: Option<LifecycleHandle>,
    operation_sequence: u64,
    retirement_requested: bool,
    owner_available: bool,
    native_settlement: NativeSettlement,
    transactions: BTreeSet<u64>,
    ingress: BTreeSet<u64>,
    provider: Option<NativeProvider>,
    pending: BTreeMap<u64, PendingOperation>,
    pub(super) resident: Option<resident::ResidentState>,
    transfer: transfer::TransferState,
    observations: watch::Sender<LifecycleSnapshot>,
    tasks: BTreeMap<u64, tokio::task::JoinHandle<()>>,
}

struct LifecycleArguments {
    identity: ActorRef,
    terminal: RetainedActorExit,
    observations: watch::Sender<LifecycleSnapshot>,
}

struct LifecycleCoordinator;

enum PendingOperation {
    Transaction(RpcReplyPort<Result<ActorAdmissionLease, KernelCallFailure>>),
    ProviderStart {
        thread: String,
        turn: String,
        reply: RpcReplyPort<Result<NativeProviderTurnLease, NativeProviderStartError>>,
    },
    TransactionCompleting {
        lease: ActorAdmissionLease,
        reply: RpcReplyPort<Result<ActorAdmissionLease, KernelCallFailure>>,
    },
    ProviderCompleting {
        reply: RpcReplyPort<Result<NativeProviderTurnLease, NativeProviderStartError>>,
    },
    Idle {
        terminal: ActorTerminal,
        reply: RpcReplyPort<Result<(), String>>,
    },
    Refusing,
}

pub(super) enum LifecycleCommand {
    AttachExecutor {
        executor: RactorRef<KernelMessage>,
        reply: RpcReplyPort<Result<(), KernelCallFailure>>,
    },
    AttachCleanupExecutor {
        executor: RactorRef<KernelMessage>,
        reply: RpcReplyPort<Result<(), KernelCallFailure>>,
    },
    ExecutorStarted(RpcReplyPort<Result<(), KernelCallFailure>>),
    ExecutorRetired,
    FinishRun {
        grant: crate::local_actor::run_control::RunRetirementGrant,
        reply: RpcReplyPort<Result<(), KernelCallFailure>>,
    },
    SuccessorDrainReceipt(RpcReplyPort<Result<SuccessorDrainReceipt, KernelCallFailure>>),
    Enqueue {
        message: Box<KernelMessage>,
        control: bool,
        reply: RpcReplyPort<Result<(), KernelCallFailure>>,
    },
    Close {
        fence: Option<Box<KernelMessage>>,
        terminal: Option<ActorTerminal>,
        reply: Option<RpcReplyPort<Result<(), KernelCallFailure>>>,
    },
    Transaction(RpcReplyPort<Result<ActorAdmissionLease, KernelCallFailure>>),
    ResourceAdmission(RpcReplyPort<Result<ActorAdmissionLease, KernelCallFailure>>),
    Source {
        delivery: crate::SourceDelivery,
        reply: Option<RpcReplyPort<Result<(), KernelCallFailure>>>,
    },
    BeginReplacement {
        grant: crate::local_actor::run_control::ReplacementIngressGrant,
        reply: RpcReplyPort<Result<(), KernelCallFailure>>,
    },
    RetargetSources {
        grant: crate::local_actor::run_control::ReplacementIngressGrant,
        reply: RpcReplyPort<Result<(), KernelCallFailure>>,
    },
    TransactionReleased(u64),
    IngressReleased(u64),
    AttachProvider {
        observation: ActorRuntimeObservationHandle,
        readiness: Arc<tidepool_runtime::session::RuntimeDurablePublicReadiness>,
        probe: Arc<dyn ProviderIdleProbe>,
        reply: RpcReplyPort<Result<(), &'static str>>,
    },
    BeginProvider {
        thread: String,
        turn: String,
        reply: RpcReplyPort<Result<NativeProviderTurnLease, NativeProviderStartError>>,
    },
    FinishProvider {
        sequence: u64,
        outcome: ProviderOutcome,
        reply: Option<RpcReplyPort<Result<(), NativeProviderSettlementError>>>,
    },
    NativeBegun {
        sequence: u64,
        outcome: Result<Option<tidepool_runtime::session::ProviderTransition>, String>,
    },
    NativeCompleted {
        sequence: u64,
        outcome: Result<(), String>,
    },
    ClaimIdle {
        terminal: ActorTerminal,
        reply: RpcReplyPort<Result<(), String>>,
    },
    IdleChecked {
        sequence: u64,
        outcome: Result<bool, String>,
    },
    Resident(Box<resident::ResidentCommand>),
    Transfer(Box<transfer::TransferCommand>),
}

pub(super) enum ProviderOutcome {
    Succeeded,
    Failed(exomonad_model::ProviderFailure),
    Interrupted,
}

/// Issued only after the coordinator has admitted this exact operation.
/// The executor retains the lease while accepted work is queued or running.
pub struct AdmittedKernelEnvelope {
    identity: ActorRef,
    sequence: u64,
    message: Box<KernelMessage>,
    lease: AdmittedKernelLease,
}

impl AdmittedKernelEnvelope {
    pub(crate) fn payload(&self) -> &KernelMessage {
        self.message.payload()
    }

    pub(crate) fn into_parts(
        self,
        expected: ActorRef,
    ) -> Result<(KernelMessage, AdmittedKernelLease), KernelCallFailure> {
        if self.identity != expected
            || self.identity != self.lease.identity
            || self.sequence != self.lease.sequence
        {
            return Err(KernelCallFailure::TargetUnavailable(expected));
        }
        Ok((*self.message, self.lease))
    }
}

pub struct AdmittedKernelLease {
    owner: RactorRef<LifecycleCommand>,
    identity: ActorRef,
    sequence: u64,
}

impl AdmittedKernelLease {
    /// Retain this original admission while the executor defers its payload.
    pub(crate) fn wrap(self, message: KernelMessage) -> KernelMessage {
        KernelMessage::Admitted(AdmittedKernelEnvelope {
            identity: self.identity,
            sequence: self.sequence,
            message: Box::new(message),
            lease: self,
        })
    }
}

impl Drop for AdmittedKernelLease {
    fn drop(&mut self) {
        let _ = self
            .owner
            .cast(LifecycleCommand::IngressReleased(self.sequence));
    }
}

/// Counts the short lease creation admission, never the subsequent Store effect.
#[must_use]
pub struct ActorAdmissionLease {
    owner: RactorRef<LifecycleCommand>,
    sequence: u64,
}

impl Drop for ActorAdmissionLease {
    fn drop(&mut self) {
        let _ = self
            .owner
            .cast(LifecycleCommand::TransactionReleased(self.sequence));
    }
}

#[derive(Clone)]
pub struct NativeProviderAdmission {
    lifecycle: LifecycleHandle,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum NativeProviderStartError {
    #[error("actor public owner installation is pending")]
    InstallationPending,
    #[error("idle retirement is being checked")]
    RetirementPending,
    #[error("actor admission is closed")]
    Closed,
    #[error("native provider round cannot start: {0}")]
    Invalid(&'static str),
    #[error("native provider transition failed: {0}")]
    Native(String),
}

pub struct NativeProviderTurnLease {
    owner: RactorRef<LifecycleCommand>,
    sequence: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NativeProviderSettlementError {
    #[error("provider lifecycle settlement owner is unavailable")]
    Unavailable,
    #[error("provider settlement does not name the active admitted turn")]
    StaleTurn,
}

impl NativeProviderTurnLease {
    pub async fn succeed(mut self) -> Result<(), NativeProviderSettlementError> {
        self.finish(ProviderOutcome::Succeeded).await
    }
    pub async fn fail(
        mut self,
        failure: exomonad_model::ProviderFailure,
    ) -> Result<(), NativeProviderSettlementError> {
        self.finish(ProviderOutcome::Failed(failure)).await
    }
    pub async fn interrupt(mut self) -> Result<(), NativeProviderSettlementError> {
        self.finish(ProviderOutcome::Interrupted).await
    }
    async fn finish(
        &mut self,
        outcome: ProviderOutcome,
    ) -> Result<(), NativeProviderSettlementError> {
        let sequence = self
            .sequence
            .take()
            .ok_or(NativeProviderSettlementError::StaleTurn)?;
        let (send, receive) = tokio::sync::oneshot::channel();
        self.owner
            .cast(LifecycleCommand::FinishProvider {
                sequence,
                outcome,
                reply: Some(send.into()),
            })
            .map_err(|_| NativeProviderSettlementError::Unavailable)?;
        receive
            .await
            .map_err(|_| NativeProviderSettlementError::Unavailable)?
    }
}

impl Drop for NativeProviderTurnLease {
    fn drop(&mut self) {
        if let Some(sequence) = self.sequence.take() {
            let _ = self.owner.cast(LifecycleCommand::FinishProvider {
                sequence,
                outcome: ProviderOutcome::Interrupted,
                reply: None,
            });
        }
    }
}

/// Logical executor retirement and resident retirement have distinct obligations.
/// Confirmed or unsupported nonresident realms never certify resident journal cleanup.
#[derive(Debug, Clone)]
enum RetirementDomain {
    NonResident,
    Resident,
}

#[derive(Debug, Clone)]
pub(crate) struct SuccessorDrainReceipt {
    domain: RetirementDomain,
    predecessor: ActorRef,
    terminal: ActorTerminal,
    cleanup: crate::ResidentCleanupOutcome,
}

impl SuccessorDrainReceipt {
    pub(crate) fn confirms_actor_retirement(&self) -> bool {
        match self.domain {
            RetirementDomain::Resident => self.cleanup.is_confirmed(),
            RetirementDomain::NonResident => {
                matches!(
                    self.cleanup.hook(),
                    crate::CleanupComponentOutcome::Confirmed
                ) && matches!(
                    self.cleanup.children(),
                    crate::CleanupComponentOutcome::Confirmed
                ) && matches!(
                    self.cleanup.realm(),
                    crate::CleanupComponentOutcome::Confirmed
                        | crate::CleanupComponentOutcome::Unsupported
                )
            }
        }
    }
    pub(crate) fn confirms_resident_retirement(&self) -> bool {
        matches!(self.domain, RetirementDomain::Resident) && self.cleanup.is_confirmed()
    }
    pub(crate) fn predecessor(&self) -> ActorRef {
        self.predecessor
    }
    pub(crate) fn terminal(&self) -> &ActorTerminal {
        &self.terminal
    }
    pub(crate) fn cleanup(&self) -> &crate::ResidentCleanupOutcome {
        &self.cleanup
    }
}

impl LifecycleHandle {
    pub async fn start(
        identity: ActorRef,
        terminal: RetainedActorExit,
    ) -> Result<Self, ractor::SpawnErr> {
        Self::start_with_supervisor(identity, terminal, None).await
    }

    pub(crate) async fn start_linked(
        identity: ActorRef,
        terminal: RetainedActorExit,
        supervisor: ractor::ActorCell,
    ) -> Result<Self, ractor::SpawnErr> {
        Self::start_with_supervisor(identity, terminal, Some(supervisor)).await
    }

    async fn start_with_supervisor(
        identity: ActorRef,
        terminal: RetainedActorExit,
        supervisor: Option<ractor::ActorCell>,
    ) -> Result<Self, ractor::SpawnErr> {
        let (observations, observe) = watch::channel(LifecycleSnapshot {
            executor: None,
            owner_available: true,
            source_forward: None,
            native_settlement: NativeSettlement::Confirmed,
            transfer_unconfirmed: None,
            phase: LifecyclePhase::Starting,
            transactions: 0,
            ingress: 0,
            provider: ProviderPhase::Detached,
            resident: None,
        });
        let arguments = LifecycleArguments {
            identity,
            terminal: terminal.clone(),
            observations,
        };
        let (address, _task) = match supervisor {
            Some(supervisor) => {
                Actor::spawn_linked(None, LifecycleCoordinator, arguments, supervisor).await?
            }
            None => Actor::spawn(None, LifecycleCoordinator, arguments).await?,
        };
        Ok(Self {
            identity,
            terminal,
            address,
            observations: observe,
            hosted_cell: Arc::new(HostedCellPublications::default()),
        })
    }

    pub(crate) async fn retire_for_successor(
        &self,
    ) -> Result<SuccessorDrainReceipt, KernelCallFailure> {
        let terminal = self.terminal.requested_shutdown().unwrap_or_else(|| {
            ActorTerminal::new(
                crate::ActorExitKind::Cancelled,
                "retired before admitting its selected successor",
            )
        });
        self.close_with(None, Some(terminal.clone())).await?;
        let (reply, _answer) = tokio::sync::oneshot::channel();
        match self
            .control(KernelMessage::Shutdown {
                terminal,
                reply: reply.into(),
            })
            .await
        {
            Ok(()) | Err(KernelCallFailure::TargetUnavailable(_)) => {}
            Err(KernelCallFailure::TargetExited(_)) if self.terminal.get().is_some() => {}
            Err(error) => return Err(error),
        }
        let mut observe = self.observations.clone();
        loop {
            let snapshot = observe.borrow_and_update().clone();
            if !snapshot.owner_available
                || snapshot.transfer_unconfirmed.is_some()
                || matches!(
                    snapshot.native_settlement,
                    NativeSettlement::NeedsAttention(_)
                )
                || snapshot
                    .resident
                    .as_ref()
                    .is_some_and(|resident| !resident.settlement.is_confirmed())
            {
                return Err(KernelCallFailure::TargetUnavailable(self.identity));
            }
            if snapshot.phase == LifecyclePhase::Retired {
                break;
            }
            observe
                .changed()
                .await
                .map_err(|_| KernelCallFailure::TargetUnavailable(self.identity))?;
        }
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.send(LifecycleCommand::SuccessorDrainReceipt(reply.into()))?;
        self.receive(answer).await?
    }

    pub fn identity(&self) -> ActorRef {
        self.identity
    }
    pub(crate) fn retained_terminal(&self) -> &RetainedActorExit {
        &self.terminal
    }

    #[cfg(test)]
    pub(crate) fn observations_for_test(&self) -> watch::Receiver<LifecycleSnapshot> {
        self.observations.clone()
    }

    pub fn observation(&self) -> LifecycleSnapshot {
        self.observations.borrow().clone()
    }
    pub(crate) fn hosted_cell(&self) -> &HostedCellSlot {
        &self.hosted_cell
    }
    pub(crate) fn resident_snapshot(&self) -> Option<resident::ResidentSnapshot> {
        self.observations.borrow().resident.clone()
    }
    pub(super) fn address(&self) -> &RactorRef<LifecycleCommand> {
        &self.address
    }

    pub async fn attach_executor(
        &self,
        executor: RactorRef<KernelMessage>,
    ) -> Result<(), KernelCallFailure> {
        let (send, receive) = tokio::sync::oneshot::channel();
        self.send(LifecycleCommand::AttachExecutor {
            executor,
            reply: send.into(),
        })?;
        self.receive(receive).await?
    }

    /// Retain physical cleanup custody without opening executor admission.
    pub(crate) async fn attach_cleanup_executor(
        &self,
        executor: RactorRef<KernelMessage>,
    ) -> Result<(), KernelCallFailure> {
        let (send, receive) = tokio::sync::oneshot::channel();
        self.send(LifecycleCommand::AttachCleanupExecutor {
            executor,
            reply: send.into(),
        })?;
        self.receive(receive).await?
    }

    pub async fn executor_started(&self) -> Result<(), KernelCallFailure> {
        let (send, receive) = tokio::sync::oneshot::channel();
        self.send(LifecycleCommand::ExecutorStarted(send.into()))?;
        self.receive(receive).await?
    }

    pub(crate) fn executor_retired(&self) {
        let _ = self.address.cast(LifecycleCommand::ExecutorRetired);
    }

    pub(crate) async fn enqueue(&self, message: KernelMessage) -> Result<(), KernelCallFailure> {
        self.enqueue_kind(message, false).await
    }

    pub(crate) async fn control(&self, message: KernelMessage) -> Result<(), KernelCallFailure> {
        self.enqueue_kind(message, true).await
    }

    async fn enqueue_kind(
        &self,
        message: KernelMessage,
        control: bool,
    ) -> Result<(), KernelCallFailure> {
        let (send, receive) = tokio::sync::oneshot::channel();
        self.send(LifecycleCommand::Enqueue {
            message: Box::new(message),
            control,
            reply: send.into(),
        })?;
        self.receive(receive).await?
    }

    pub(crate) fn dispatch_source(
        &self,
        delivery: crate::SourceDelivery,
    ) -> Result<(), KernelCallFailure> {
        self.send(LifecycleCommand::Source {
            delivery,
            reply: None,
        })
    }

    pub(crate) fn source_recipient(&self) -> ActorRef {
        let mut recipient = self.clone();
        loop {
            let next = recipient.observations.borrow().source_forward.clone();
            match next {
                Some(next) => recipient = next,
                None => return recipient.identity,
            }
        }
    }

    pub(crate) async fn begin_replacement(
        &self,
        grant: crate::local_actor::run_control::ReplacementIngressGrant,
    ) -> Result<(), KernelCallFailure> {
        let (send, receive) = tokio::sync::oneshot::channel();
        self.send(LifecycleCommand::BeginReplacement {
            grant,
            reply: send.into(),
        })?;
        self.receive(receive).await?
    }

    pub(crate) async fn retarget_sources(
        &self,
        grant: crate::local_actor::run_control::ReplacementIngressGrant,
    ) -> Result<(), KernelCallFailure> {
        let (send, receive) = tokio::sync::oneshot::channel();
        self.send(LifecycleCommand::RetargetSources {
            grant,
            reply: send.into(),
        })?;
        self.receive(receive).await?
    }

    /// Consume the run owner's exact finalization authority and await owner exit.
    pub(crate) async fn finish_run(
        &self,
        grant: crate::local_actor::run_control::RunRetirementGrant,
    ) -> Result<(), KernelCallFailure> {
        let (send, receive) = tokio::sync::oneshot::channel();
        // A retirement completion can precede the executor's physical stop.
        // This cell observation grants no execution; the owner rechecks below.
        let executor = self.observations.borrow().executor.clone();
        if let Some(executor) = executor {
            executor
                .get_cell()
                .wait(None)
                .await
                .map_err(|_| KernelCallFailure::TargetUnavailable(self.identity))?;
        }
        self.send(LifecycleCommand::FinishRun {
            grant,
            reply: send.into(),
        })?;
        self.receive(receive).await??;
        self.address
            .get_cell()
            .wait(None)
            .await
            .map_err(|_| KernelCallFailure::TargetUnavailable(self.identity))
    }

    #[cfg(test)]
    pub(crate) async fn stop_owner_for_test(&self) {
        self.address.stop(Some("test lifecycle owner loss".into()));
        self.address
            .get_cell()
            .wait(Some(std::time::Duration::from_secs(30)))
            .await
            .expect("lifecycle stop completes");
        assert_eq!(self.address.get_status(), ractor::ActorStatus::Stopped);
    }

    pub(crate) fn run_finalized(&self) -> bool {
        self.observations.borrow().phase == LifecyclePhase::Finalized
            && self.address.get_status() == ractor::ActorStatus::Stopped
    }

    pub async fn close(&self) -> Result<(), KernelCallFailure> {
        self.close_with(None, None).await
    }

    pub(crate) async fn close_with_fence(
        &self,
        fence: KernelMessage,
    ) -> Result<(), KernelCallFailure> {
        self.close_with(Some(fence), None).await
    }

    pub(crate) fn dispatch_shutdown(
        &self,
        terminal: ActorTerminal,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<(), KernelCallFailure>>, KernelCallFailure>
    {
        let (send, receive) = tokio::sync::oneshot::channel();
        self.send(LifecycleCommand::Close {
            fence: None,
            terminal: Some(terminal),
            reply: Some(send.into()),
        })?;
        Ok(receive)
    }

    async fn close_with(
        &self,
        fence: Option<KernelMessage>,
        terminal: Option<ActorTerminal>,
    ) -> Result<(), KernelCallFailure> {
        let (send, receive) = tokio::sync::oneshot::channel();
        self.send(LifecycleCommand::Close {
            fence: fence.map(Box::new),
            terminal,
            reply: Some(send.into()),
        })?;
        self.receive(receive).await?
    }

    pub(crate) fn close_abandoned(&self) {
        let _ = self.address.cast(LifecycleCommand::Close {
            fence: None,
            terminal: None,
            reply: None,
        });
    }

    pub async fn wait_transactions(&self) -> Result<(), KernelCallFailure> {
        let mut observe = self.observations.clone();
        loop {
            let snapshot = observe.borrow_and_update().clone();
            if snapshot.transfer_unconfirmed.is_some()
                || !snapshot.owner_available
                || matches!(
                    snapshot.native_settlement,
                    NativeSettlement::NeedsAttention(_)
                )
                || snapshot
                    .resident
                    .as_ref()
                    .is_some_and(|resident| !resident.settlement.is_confirmed())
            {
                return Err(KernelCallFailure::TargetUnavailable(self.identity));
            }
            if snapshot.transactions == 0 {
                return Ok(());
            }
            if observe.changed().await.is_err() {
                return Err(KernelCallFailure::TargetUnavailable(self.identity));
            }
        }
    }

    /// Internal executor resource publication is allowed during bootstrap.
    /// This lease counts local ownership; native Store effects retain their
    /// original separate admission contract.
    pub(crate) async fn resource_admission(
        &self,
    ) -> Result<ActorAdmissionLease, KernelCallFailure> {
        let (send, receive) = tokio::sync::oneshot::channel();
        self.send(LifecycleCommand::ResourceAdmission(send.into()))?;
        self.receive(receive).await?
    }

    pub async fn transaction(&self) -> Result<ActorAdmissionLease, KernelCallFailure> {
        let (send, receive) = tokio::sync::oneshot::channel();
        self.send(LifecycleCommand::Transaction(send.into()))?;
        self.receive(receive).await?
    }

    pub async fn attach_provider(
        &self,
        observation: ActorRuntimeObservationHandle,
        readiness: Arc<tidepool_runtime::session::RuntimeDurablePublicReadiness>,
        probe: Arc<dyn ProviderIdleProbe>,
    ) -> Result<NativeProviderAdmission, &'static str> {
        let (send, receive) = tokio::sync::oneshot::channel();
        self.address
            .cast(LifecycleCommand::AttachProvider {
                observation,
                readiness,
                probe,
                reply: send.into(),
            })
            .map_err(|_| "actor lifecycle is unavailable")?;
        receive
            .await
            .map_err(|_| "actor lifecycle is unavailable")??;
        Ok(NativeProviderAdmission {
            lifecycle: self.clone(),
        })
    }

    pub(crate) async fn claim_idle(&self, terminal: ActorTerminal) -> Result<(), String> {
        let (send, receive) = tokio::sync::oneshot::channel();
        self.address
            .cast(LifecycleCommand::ClaimIdle {
                terminal,
                reply: send.into(),
            })
            .map_err(|error| error.to_string())?;
        receive
            .await
            .map_err(|_| "idle retirement outcome unavailable".to_string())?
    }

    fn send(&self, command: LifecycleCommand) -> Result<(), KernelCallFailure> {
        self.address
            .cast(command)
            .map_err(|_| KernelCallFailure::TargetExited(self.identity))
    }
    async fn receive<T>(
        &self,
        receive: tokio::sync::oneshot::Receiver<T>,
    ) -> Result<T, KernelCallFailure> {
        receive
            .await
            .map_err(|_| KernelCallFailure::TargetExited(self.identity))
    }
}

impl NativeProviderAdmission {
    pub async fn wait_for_retirement_decision(&self) {
        let mut observe = self.lifecycle.observations.clone();
        loop {
            if observe.borrow_and_update().phase != LifecyclePhase::IdleCheck {
                return;
            }
            if observe.changed().await.is_err() {
                return;
            }
        }
    }
    pub async fn begin_provider_turn(
        &self,
        thread: String,
        turn: String,
    ) -> Result<NativeProviderTurnLease, NativeProviderStartError> {
        let (send, receive) = tokio::sync::oneshot::channel();
        self.lifecycle
            .address
            .cast(LifecycleCommand::BeginProvider {
                thread,
                turn,
                reply: send.into(),
            })
            .map_err(|_| NativeProviderStartError::Closed)?;
        receive
            .await
            .map_err(|_| NativeProviderStartError::Closed)?
    }
}

impl LifecycleState {
    pub(super) fn has_pending(&self) -> bool {
        !self.transactions.is_empty()
            || !self.ingress.is_empty()
            || !self.pending.is_empty()
            || self.transfer.has_pending()
            || self
                .resident
                .as_ref()
                .is_some_and(resident::ResidentState::has_pending)
    }

    pub(super) fn is_closed(&self) -> bool {
        matches!(
            self.phase,
            LifecyclePhase::Closing | LifecyclePhase::Retired | LifecyclePhase::Finalized
        )
    }

    pub(super) fn next_operation(&mut self) -> u64 {
        self.next_sequence()
            .expect("lifecycle operation sequence exhausted")
    }

    pub(super) fn publish(&self) {
        self.observations.send_replace(LifecycleSnapshot {
            executor: self.executor.clone(),
            owner_available: self.owner_available,
            source_forward: self.source_forward.clone(),
            native_settlement: self.native_settlement.clone(),
            transfer_unconfirmed: self.transfer.unconfirmed.clone(),
            phase: self.phase,
            transactions: self.transactions.len()
                + self.transfer.pending_count()
                + self
                    .pending
                    .values()
                    .filter(|pending| {
                        !matches!(pending, PendingOperation::TransactionCompleting { .. })
                    })
                    .count()
                + usize::from(
                    self.resident
                        .as_ref()
                        .is_some_and(resident::ResidentState::has_pending),
                ),
            ingress: self.ingress.len(),
            provider: self
                .provider
                .as_ref()
                .map_or(ProviderPhase::Detached, |provider| provider.phase),
            resident: self.resident.as_ref().map(resident::snapshot),
        });
    }

    fn next_sequence(&mut self) -> Option<u64> {
        self.operation_sequence = self.operation_sequence.checked_add(1)?;
        Some(self.operation_sequence)
    }

    fn refusal(&self) -> Option<KernelCallFailure> {
        if self.transfer.unconfirmed.is_some() {
            return Some(KernelCallFailure::TargetUnavailable(self.identity));
        }
        if self
            .resident
            .as_ref()
            .and_then(resident::settlement_refusal)
            .is_some()
        {
            return Some(KernelCallFailure::TargetUnavailable(self.identity));
        }
        if matches!(self.native_settlement, NativeSettlement::NeedsAttention(_)) {
            return Some(KernelCallFailure::TargetUnavailable(self.identity));
        }
        match self.phase {
            LifecyclePhase::Open => None,
            LifecyclePhase::Installing => {
                Some(KernelCallFailure::InstallationPending(self.identity))
            }
            LifecyclePhase::IdleCheck => Some(KernelCallFailure::RetirementPending(self.identity)),
            LifecyclePhase::Starting => Some(KernelCallFailure::TargetUnavailable(self.identity)),
            LifecyclePhase::Closing => Some(KernelCallFailure::MailboxClosed(self.identity)),
            LifecyclePhase::Retired => Some(KernelCallFailure::TargetExited(self.identity)),
            LifecyclePhase::Finalized => Some(KernelCallFailure::TargetUnavailable(self.identity)),
        }
    }

    fn enqueue(
        &mut self,
        myself: &RactorRef<LifecycleCommand>,
        message: KernelMessage,
        control: bool,
    ) -> Result<(), KernelCallFailure> {
        if !control {
            if let Some(refusal) = self.refusal() {
                return Err(refusal);
            }
        }
        if matches!(
            self.phase,
            LifecyclePhase::Retired | LifecyclePhase::Finalized
        ) {
            return Err(KernelCallFailure::TargetExited(self.identity));
        }
        let executor = self
            .executor
            .clone()
            .ok_or(KernelCallFailure::TargetUnavailable(self.identity))?;
        let sequence = self
            .next_sequence()
            .ok_or(KernelCallFailure::MailboxClosed(self.identity))?;
        self.ingress.insert(sequence);
        let envelope = AdmittedKernelEnvelope {
            identity: self.identity,
            sequence,
            message: Box::new(message),
            lease: AdmittedKernelLease {
                owner: myself.clone(),
                identity: self.identity,
                sequence,
            },
        };
        executor
            .cast(KernelMessage::Admitted(envelope))
            .map_err(|_| KernelCallFailure::TargetExited(self.identity))
    }

    fn provider_error(&self) -> Option<NativeProviderStartError> {
        if let Some(detail) = &self.transfer.unconfirmed {
            return Some(NativeProviderStartError::Native(detail.clone()));
        }
        if let Some(detail) = self
            .resident
            .as_ref()
            .and_then(resident::settlement_refusal)
        {
            return Some(NativeProviderStartError::Native(detail.to_owned()));
        }
        if let NativeSettlement::NeedsAttention(detail) = &self.native_settlement {
            return Some(NativeProviderStartError::Native(detail.clone()));
        }
        match self.phase {
            LifecyclePhase::Open => None,
            LifecyclePhase::Installing => Some(NativeProviderStartError::InstallationPending),
            LifecyclePhase::IdleCheck => Some(NativeProviderStartError::RetirementPending),
            _ => Some(NativeProviderStartError::Closed),
        }
    }
}

impl Actor for LifecycleCoordinator {
    type Msg = LifecycleCommand;
    type State = LifecycleState;
    type Arguments = LifecycleArguments;

    async fn pre_start(
        &self,
        _myself: RactorRef<Self::Msg>,
        arguments: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(LifecycleState {
            identity: arguments.identity,
            terminal: arguments.terminal,
            phase: LifecyclePhase::Starting,
            executor: None,
            hosted_sealed: false,
            source_forward: None,
            operation_sequence: 0,
            retirement_requested: false,
            owner_available: true,
            native_settlement: NativeSettlement::Confirmed,
            transactions: BTreeSet::new(),
            ingress: BTreeSet::new(),
            provider: None,
            pending: BTreeMap::new(),
            resident: None,
            transfer: transfer::TransferState::default(),
            observations: arguments.observations,
            tasks: BTreeMap::new(),
        })
    }

    async fn handle(
        &self,
        myself: RactorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            LifecycleCommand::AttachExecutor { executor, reply } => {
                let result = if state.phase == LifecyclePhase::Starting && state.executor.is_none()
                {
                    state.executor = Some(executor);
                    Ok(())
                } else {
                    Err(KernelCallFailure::MailboxClosed(state.identity))
                };
                let _ = reply.send(result);
            }
            LifecycleCommand::AttachCleanupExecutor { executor, reply } => {
                let permitted_phase = matches!(
                    state.phase,
                    LifecyclePhase::Starting | LifecyclePhase::Closing
                );
                let exact = state
                    .executor
                    .as_ref()
                    .is_none_or(|retained| retained.get_id() == executor.get_id());
                let result = if permitted_phase && exact {
                    state.executor = Some(executor);
                    Ok(())
                } else {
                    Err(KernelCallFailure::TargetUnavailable(state.identity))
                };
                // Retaining a cell does not change lifecycle phase or admit work.
                let _ = reply.send(result);
            }
            LifecycleCommand::ExecutorStarted(reply) => {
                let result = if state.phase == LifecyclePhase::Starting && state.executor.is_some()
                {
                    state.phase = LifecyclePhase::Open;
                    Ok(())
                } else {
                    Err(KernelCallFailure::MailboxClosed(state.identity))
                };
                let _ = reply.send(result);
            }
            LifecycleCommand::FinishRun { grant, reply } => {
                let exact = grant.actor() == state.identity
                    && grant.lifecycle().address.get_id() == myself.get_id();
                let settled = !state.has_pending()
                    && state.tasks.is_empty()
                    && matches!(state.native_settlement, NativeSettlement::Confirmed)
                    && state.transfer.unconfirmed.is_none();
                let retired = state.phase == LifecyclePhase::Retired
                    && state.executor.as_ref().is_some_and(|executor| {
                        executor.get_status() == ractor::ActorStatus::Stopped
                    })
                    && state.terminal.get().is_some()
                    && state
                        .resident
                        .as_ref()
                        .is_none_or(resident::retirement_confirmed)
                    && state.terminal.cleanup().is_some_and(|cleanup| {
                        cleanup.actor() == state.identity
                            && if state.resident.is_some() {
                                cleanup.is_confirmed()
                            } else {
                                matches!(cleanup.hook(), crate::CleanupComponentOutcome::Confirmed)
                                    && matches!(
                                        cleanup.children(),
                                        crate::CleanupComponentOutcome::Confirmed
                                    )
                                    && matches!(
                                        cleanup.realm(),
                                        crate::CleanupComponentOutcome::Confirmed
                                            | crate::CleanupComponentOutcome::Unsupported
                                    )
                            }
                    });
                let never_registered = state.phase == LifecyclePhase::Closing
                    && state.executor.is_none()
                    && state.resident.is_none();
                if exact
                    && settled
                    && state.owner_available
                    && if grant.never_registered() {
                        never_registered || retired
                    } else {
                        retired
                    }
                {
                    state.phase = LifecyclePhase::Finalized;
                    // The owned effect survives a dropped finalization observer.
                    let _ = reply.send(Ok(()));
                    myself.stop(Some("confirmed run finalization".into()));
                } else {
                    let _ = reply.send(Err(KernelCallFailure::TargetUnavailable(state.identity)));
                }
            }
            LifecycleCommand::ExecutorRetired => {
                state.retirement_requested = true;
                state.phase = LifecyclePhase::Closing;
                // Retain the exact cell for finalization, including forgotten membership.
                // Closing/Retired phases already prohibit new executor ingress.
                if let Some(provider) = state.provider.as_mut() {
                    provider.turn.take();
                    provider.phase = ProviderPhase::NeedsAttention;
                }
            }
            LifecycleCommand::SuccessorDrainReceipt(reply) => {
                let resident_confirmed = state
                    .resident
                    .as_ref()
                    .is_none_or(resident::retirement_confirmed);
                let result = match (state.phase, state.terminal.get(), state.terminal.cleanup()) {
                    (LifecyclePhase::Retired, Some(terminal), Some(cleanup))
                        if state.owner_available
                            && resident_confirmed
                            && matches!(state.native_settlement, NativeSettlement::Confirmed)
                            && state.transfer.unconfirmed.is_none()
                            && cleanup.actor() == state.identity
                            && if state.resident.is_some() {
                                cleanup.is_confirmed()
                            } else {
                                matches!(cleanup.hook(), crate::CleanupComponentOutcome::Confirmed)
                                    && matches!(
                                        cleanup.children(),
                                        crate::CleanupComponentOutcome::Confirmed
                                    )
                                    && matches!(
                                        cleanup.realm(),
                                        crate::CleanupComponentOutcome::Confirmed
                                            | crate::CleanupComponentOutcome::Unsupported
                                    )
                            } =>
                    {
                        Ok(SuccessorDrainReceipt {
                            domain: if state.resident.is_some() {
                                RetirementDomain::Resident
                            } else {
                                RetirementDomain::NonResident
                            },
                            predecessor: state.identity,
                            terminal,
                            cleanup,
                        })
                    }
                    _ => Err(KernelCallFailure::TargetUnavailable(state.identity)),
                };
                let _ = reply.send(result);
            }
            LifecycleCommand::Source { delivery, reply } => {
                // Cast directly along the selected chain: spawning per-event tasks
                // would reorder publications relative to the replacement fence.
                if let Some(next) = &state.source_forward {
                    let _ = next
                        .address
                        .cast(LifecycleCommand::Source { delivery, reply });
                } else {
                    let result = state.enqueue(&myself, KernelMessage::Source(delivery), false);
                    if let Some(reply) = reply {
                        let _ = reply.send(result);
                    }
                }
            }
            LifecycleCommand::BeginReplacement { grant, reply } => {
                let result = if grant.predecessor() != state.identity
                    || state.phase != LifecyclePhase::Open
                {
                    Err(KernelCallFailure::MailboxClosed(state.identity))
                } else {
                    state.source_forward = Some(grant.successor_handle().clone());
                    state.phase = LifecyclePhase::Closing;
                    state.enqueue(&myself, KernelMessage::ReplacementFence, true)
                };
                state.publish();
                let _ = reply.send(result);
            }
            LifecycleCommand::RetargetSources { grant, reply } => {
                // Keep the forwarding path intact. Shortening A→B→C to A→C
                // can overtake an earlier event still in B's mailbox.
                let current = state
                    .source_forward
                    .as_ref()
                    .map_or(state.identity, LifecycleHandle::source_recipient);
                let result = if current == grant.successor() {
                    Ok(())
                } else {
                    Err(KernelCallFailure::TargetUnavailable(state.identity))
                };
                let _ = reply.send(result);
            }
            LifecycleCommand::Enqueue {
                message,
                control,
                reply,
            } => {
                if matches!(message.payload(), KernelMessage::SealHostedWork { .. }) {
                    state.hosted_sealed = true;
                }
                let result = if state.hosted_sealed
                    && matches!(
                        message.payload(),
                        KernelMessage::Tool { .. }
                            | KernelMessage::ToolWithHostedCheckpoint { .. }
                            | KernelMessage::Workbench { .. }
                    ) {
                    Err(KernelCallFailure::MailboxClosed(state.identity))
                } else {
                    state.enqueue(&myself, *message, control)
                };
                let _ = reply.send(result);
            }
            LifecycleCommand::Close {
                fence,
                terminal,
                reply,
            } => {
                if let Some(terminal) = terminal {
                    state.terminal.request_shutdown(terminal);
                }
                if !matches!(
                    state.phase,
                    LifecyclePhase::Retired | LifecyclePhase::Finalized
                ) {
                    state.phase = LifecyclePhase::Closing;
                }
                let result = match fence {
                    Some(fence) => state.enqueue(&myself, *fence, true),
                    None => Ok(()),
                };
                if let Some(reply) = reply {
                    let _ = reply.send(result);
                }
            }
            LifecycleCommand::Transaction(reply) => {
                if let Some(refusal) = state.refusal() {
                    let _ = reply.send(Err(refusal));
                } else {
                    let sequence = state
                        .next_sequence()
                        .ok_or("lifecycle sequence exhausted")?;
                    state
                        .pending
                        .insert(sequence, PendingOperation::Transaction(reply));
                    begin_native(state, &myself, sequence);
                }
            }
            LifecycleCommand::ResourceAdmission(reply) => {
                let startup = state.phase == LifecyclePhase::Starting
                    && matches!(state.native_settlement, NativeSettlement::Confirmed)
                    && state.transfer.unconfirmed.is_none()
                    && state
                        .resident
                        .as_ref()
                        .and_then(resident::settlement_refusal)
                        .is_none();
                let refusal = if startup { None } else { state.refusal() };
                if let Some(refusal) = refusal {
                    let _ = reply.send(Err(refusal));
                } else {
                    let sequence = state.next_operation();
                    state.transactions.insert(sequence);
                    let _ = reply.send(Ok(ActorAdmissionLease {
                        owner: myself.clone(),
                        sequence,
                    }));
                }
            }
            LifecycleCommand::TransactionReleased(sequence) => {
                state.transactions.remove(&sequence);
            }
            LifecycleCommand::IngressReleased(sequence) => {
                state.ingress.remove(&sequence);
            }
            LifecycleCommand::AttachProvider {
                observation,
                readiness,
                probe,
                reply,
            } => {
                let result = if state.phase == LifecyclePhase::Open && state.provider.is_none() {
                    state.provider = Some(NativeProvider {
                        phase: ProviderPhase::NotStarted,
                        observation,
                        readiness,
                        probe,
                        turn: None,
                        sequence: None,
                    });
                    Ok(())
                } else {
                    Err("native provider cannot attach to a closed or already attached actor")
                };
                let _ = reply.send(result);
            }
            LifecycleCommand::BeginProvider {
                thread,
                turn,
                reply,
            } => {
                let refusal = state
                    .provider_error()
                    .or_else(|| match state.provider.as_ref() {
                        None => Some(NativeProviderStartError::Invalid(
                            "native provider is not attached",
                        )),
                        Some(provider) if provider.phase == ProviderPhase::Active => {
                            Some(NativeProviderStartError::Invalid(
                                "a native provider round is already active",
                            ))
                        }
                        _ if state.pending.values().any(|pending| {
                            matches!(
                                pending,
                                PendingOperation::ProviderStart { .. }
                                    | PendingOperation::ProviderCompleting { .. }
                            )
                        }) =>
                        {
                            Some(NativeProviderStartError::Invalid(
                                "a native provider transition is pending",
                            ))
                        }
                        _ => None,
                    });
                if let Some(refusal) = refusal {
                    let _ = reply.send(Err(refusal));
                } else {
                    let sequence = state
                        .next_sequence()
                        .ok_or("lifecycle sequence exhausted")?;
                    state.pending.insert(
                        sequence,
                        PendingOperation::ProviderStart {
                            thread,
                            turn,
                            reply,
                        },
                    );
                    begin_native(state, &myself, sequence);
                }
            }
            LifecycleCommand::FinishProvider {
                sequence,
                outcome,
                reply,
            } => {
                let mut result = Err(NativeProviderSettlementError::StaleTurn);
                if let Some(provider) = state.provider.as_mut() {
                    if provider.sequence == Some(sequence) {
                        if let Some(turn) = provider.turn.take() {
                            provider.phase = match outcome {
                                ProviderOutcome::Succeeded => {
                                    turn.succeed();
                                    ProviderPhase::Idle
                                }
                                ProviderOutcome::Failed(failure) => {
                                    turn.fail(failure);
                                    ProviderPhase::NeedsAttention
                                }
                                ProviderOutcome::Interrupted => {
                                    turn.interrupt();
                                    ProviderPhase::NeedsAttention
                                }
                            };
                        }
                        provider.sequence = None;
                        result = Ok(());
                    }
                }
                if let Some(reply) = reply {
                    let _ = reply.send(result);
                }
            }
            LifecycleCommand::NativeBegun { sequence, outcome } => {
                if matches!(
                    state.pending.get(&sequence),
                    Some(PendingOperation::Transaction(_) | PendingOperation::ProviderStart { .. })
                ) {
                    state.tasks.remove(&sequence);
                    native_begun(state, &myself, sequence, outcome);
                } else if matches!(&outcome, Ok(Some(_))) {
                    // A live unmatched native ticket will be abandoned by Drop.
                    // Its native owner fences; retain the uncertainty locally
                    // without rewriting a later provider turn's observation.
                    state.native_settlement = NativeSettlement::NeedsAttention(
                        "native admission returned an unmatched live transition".into(),
                    );
                }
            }
            LifecycleCommand::NativeCompleted { sequence, outcome } => {
                if matches!(
                    state.pending.get(&sequence),
                    Some(
                        PendingOperation::TransactionCompleting { .. }
                            | PendingOperation::ProviderCompleting { .. }
                            | PendingOperation::Refusing
                    )
                ) {
                    state.tasks.remove(&sequence);
                    native_completed(state, &myself, sequence, outcome);
                } else if sequence > state.operation_sequence {
                    state.native_settlement = NativeSettlement::NeedsAttention(
                        "native completion names an operation that was never issued".into(),
                    );
                }
            }
            LifecycleCommand::ClaimIdle { terminal, reply } => {
                let eligible = state.phase == LifecyclePhase::Open
                    && state.transactions.is_empty()
                    && state.ingress.is_empty()
                    && state.pending.is_empty()
                    && matches!(state.native_settlement, NativeSettlement::Confirmed)
                    && !state
                        .resident
                        .as_ref()
                        .is_some_and(resident::ResidentState::has_pending)
                    && state
                        .provider
                        .as_ref()
                        .is_some_and(|provider| provider.phase == ProviderPhase::Idle);
                if !eligible {
                    let _ = reply.send(Err(
                        "actor has admitted work or is not confirmed idle".into()
                    ));
                } else {
                    state.phase = LifecyclePhase::IdleCheck;
                    let sequence = state
                        .next_sequence()
                        .ok_or("lifecycle sequence exhausted")?;
                    let probe = state
                        .provider
                        .as_ref()
                        .expect("idle provider")
                        .probe
                        .clone();
                    state
                        .pending
                        .insert(sequence, PendingOperation::Idle { terminal, reply });
                    let owner = myself.clone();
                    state.tasks.insert(
                        sequence,
                        tokio::spawn(async move {
                            let outcome =
                                match AssertUnwindSafe(async move { probe.check_idle().await })
                                    .catch_unwind()
                                    .await
                                {
                                    Ok(outcome) => outcome,
                                    Err(_) => Err("idle probe worker panicked".into()),
                                };
                            let _ = owner.cast(LifecycleCommand::IdleChecked { sequence, outcome });
                        }),
                    );
                }
            }
            LifecycleCommand::IdleChecked { sequence, outcome } => {
                state.tasks.remove(&sequence);
                if let Some(PendingOperation::Idle { terminal, reply }) =
                    state.pending.remove(&sequence)
                {
                    let result = if state.phase != LifecyclePhase::IdleCheck {
                        Err("actor closed while idle retirement was checked".into())
                    } else {
                        match outcome {
                            Ok(true) => {
                                state.phase = LifecyclePhase::Closing;
                                let terminal = state.terminal.request_shutdown(terminal);
                                let (send, _receive) = tokio::sync::oneshot::channel();
                                state
                                    .enqueue(
                                        &myself,
                                        KernelMessage::Shutdown {
                                            terminal,
                                            reply: send.into(),
                                        },
                                        true,
                                    )
                                    .map_err(|error| error.to_string())
                            }
                            Ok(false) => {
                                state.phase = LifecyclePhase::Open;
                                Err(
                                    "native provider has pending durable work or lost its binding"
                                        .into(),
                                )
                            }
                            Err(error) => {
                                state.phase = LifecyclePhase::Open;
                                Err(error)
                            }
                        }
                    };
                    let _ = reply.send(result);
                }
            }
            LifecycleCommand::Resident(command) => resident::handle(state, &myself, *command),
            LifecycleCommand::Transfer(command) => transfer::handle(state, &myself, *command),
        }
        if state.retirement_requested
            && !state.has_pending()
            && state.phase != LifecyclePhase::Finalized
        {
            state.phase = LifecyclePhase::Retired;
        }
        state.publish();
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        _myself: RactorRef<Self::Msg>,
        _event: ractor::SupervisionEvent,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        // Native workers are owned Tokio tasks with correlated completion.
        // A sibling's terminal signal cannot settle another operation.
        Ok(())
    }

    async fn post_stop(
        &self,
        _myself: RactorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        if state.phase != LifecyclePhase::Finalized {
            state.phase = LifecyclePhase::Retired;
        }
        state.owner_available = false;
        for task in state.tasks.values() {
            task.abort();
        }
        // Retain unresolved counts: aborting an owned task is not proof that
        // its native publication or cancellation has settled.
        state.provider.take();
        // Release durable boot custody before the coordinator stop acknowledgement.
        state.resident.take();
        state.publish();
        Ok(())
    }
}

fn begin_native(state: &mut LifecycleState, myself: &RactorRef<LifecycleCommand>, sequence: u64) {
    let readiness = match state.provider.as_ref() {
        Some(provider) => Ok(Some(provider.readiness.clone())),
        None => match state.resident.as_ref() {
            Some(resident) => resident::native_readiness(resident),
            None => Ok(None),
        },
    };
    let owner = myself.clone();
    state.tasks.insert(
        sequence,
        tokio::spawn(async move {
            let outcome = match readiness {
                Ok(Some(readiness)) => match AssertUnwindSafe(async move {
                    readiness
                        .begin_provider_transition()
                        .await
                        .map(Some)
                        .map_err(|error| error.to_string())
                })
                .catch_unwind()
                .await
                {
                    Ok(outcome) => outcome,
                    Err(_) => Err("native admission worker panicked".into()),
                },
                Err(error) => Err(error),
                Ok(None) => Ok(None),
            };
            let _ = owner.cast(LifecycleCommand::NativeBegun { sequence, outcome });
        }),
    );
}

fn native_begun(
    state: &mut LifecycleState,
    myself: &RactorRef<LifecycleCommand>,
    sequence: u64,
    outcome: Result<Option<tidepool_runtime::session::ProviderTransition>, String>,
) {
    let Some(pending) = state.pending.remove(&sequence) else {
        // A stale local completion has no authority over a later turn. An
        // unmatched native ticket is dropped, fencing its issuing native owner.
        return;
    };
    let transition = match outcome {
        Ok(transition) => transition,
        Err(error) => {
            refuse_pending(state, pending, error);
            return;
        }
    };
    if let Some(refusal) = state.refusal() {
        refuse_pending(state, pending, refusal.to_string());
        if let Some(transition) = transition {
            launch_refuse(state, myself, sequence, transition);
        }
        return;
    }
    // This is the admission cutoff. The native Begin has succeeded and the
    // coordinator now creates the actual local lease or provider observation.
    // A later Close fences new work but must let this exact admitted operation
    // settle native Complete before its success reply or retirement drain.
    let completing = match pending {
        PendingOperation::Transaction(reply) => {
            state.transactions.insert(sequence);
            PendingOperation::TransactionCompleting {
                lease: ActorAdmissionLease {
                    owner: myself.clone(),
                    sequence,
                },
                reply,
            }
        }
        PendingOperation::ProviderStart {
            thread,
            turn,
            reply,
        } => {
            let Some(provider) = state.provider.as_mut() else {
                return;
            };
            match provider.observation.begin_provider_turn(thread, turn) {
                Ok(turn) => {
                    provider.turn = Some(turn);
                    provider.phase = ProviderPhase::Active;
                    provider.sequence = Some(sequence);
                    PendingOperation::ProviderCompleting { reply }
                }
                Err(error) => {
                    let _ = reply.send(Err(NativeProviderStartError::Invalid(error)));
                    if let Some(transition) = transition {
                        launch_refuse(state, myself, sequence, transition);
                    }
                    return;
                }
            }
        }
        other => {
            refuse_pending(
                state,
                other,
                "native completion has wrong operation phase".into(),
            );
            return;
        }
    };
    state.pending.insert(sequence, completing);
    let Some(transition) = transition else {
        native_completed(state, myself, sequence, Ok(()));
        return;
    };
    let owner = myself.clone();
    state.tasks.insert(
        sequence,
        tokio::spawn(async move {
            let outcome = match AssertUnwindSafe(async move {
                transition
                    .complete()
                    .await
                    .map_err(|error| error.to_string())
            })
            .catch_unwind()
            .await
            {
                Ok(outcome) => outcome,
                Err(_) => Err("native completion worker panicked".into()),
            };
            let _ = owner.cast(LifecycleCommand::NativeCompleted { sequence, outcome });
        }),
    );
}

fn native_completed(
    state: &mut LifecycleState,
    myself: &RactorRef<LifecycleCommand>,
    sequence: u64,
    outcome: Result<(), String>,
) {
    let Some(pending) = state.pending.remove(&sequence) else {
        // A stale local completion has no authority over a later turn. An
        // unmatched native ticket is dropped, fencing its issuing native owner.
        return;
    };
    match (pending, outcome) {
        (PendingOperation::Refusing, Ok(())) => {}
        (PendingOperation::TransactionCompleting { lease, reply }, Ok(())) => {
            let _ = reply.send(Ok(lease));
        }
        (PendingOperation::ProviderCompleting { reply }, Ok(())) => {
            let _ = reply.send(Ok(NativeProviderTurnLease {
                owner: myself.clone(),
                sequence: Some(sequence),
            }));
        }
        (pending, Err(error)) => {
            state.native_settlement = NativeSettlement::NeedsAttention(error.clone());
            refuse_pending(state, pending, error);
        }
        (pending, Ok(())) => {
            state.native_settlement = NativeSettlement::NeedsAttention(
                "native completion has wrong operation phase".into(),
            );
            refuse_pending(
                state,
                pending,
                "native completion has wrong operation phase".into(),
            );
        }
    }
}

fn refuse_pending(state: &mut LifecycleState, pending: PendingOperation, error: String) {
    if let Some(provider) = state.provider.as_mut() {
        provider.phase = ProviderPhase::NeedsAttention;
        provider.turn.take();
        provider.sequence = None;
    }
    match pending {
        PendingOperation::Transaction(reply)
        | PendingOperation::TransactionCompleting { reply, .. } => {
            let _ = reply.send(Err(KernelCallFailure::TargetUnavailable(state.identity)));
        }
        PendingOperation::ProviderStart { reply, .. }
        | PendingOperation::ProviderCompleting { reply } => {
            let _ = reply.send(Err(NativeProviderStartError::Native(error)));
        }
        PendingOperation::Idle { reply, .. } => {
            let _ = reply.send(Err(error));
        }
        PendingOperation::Refusing => {}
    }
}

fn launch_refuse(
    state: &mut LifecycleState,
    myself: &RactorRef<LifecycleCommand>,
    sequence: u64,
    transition: tidepool_runtime::session::ProviderTransition,
) {
    state.pending.insert(sequence, PendingOperation::Refusing);
    let owner = myself.clone();
    state.tasks.insert(
        sequence,
        tokio::spawn(async move {
            let outcome = match AssertUnwindSafe(async move {
                transition.refuse().await.map_err(|error| error.to_string())
            })
            .catch_unwind()
            .await
            {
                Ok(outcome) => outcome,
                Err(_) => Err("native refusal worker panicked".into()),
            };
            let _ = owner.cast(LifecycleCommand::NativeCompleted { sequence, outcome });
        }),
    );
}

impl std::fmt::Debug for LifecycleHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LifecycleHandle")
            .field("identity", &self.identity)
            .field("phase", &self.observations.borrow().phase)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for ActorAdmissionLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ActorAdmissionLease")
            .field("sequence", &self.sequence)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for NativeProviderTurnLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeProviderTurnLease")
            .field("sequence", &self.sequence)
            .finish_non_exhaustive()
    }
}
