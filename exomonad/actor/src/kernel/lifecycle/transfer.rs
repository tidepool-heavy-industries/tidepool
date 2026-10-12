//! Affine custody transfer along a RunControl-issued replacement relationship.
use super::*;
use crate::local_actor::run_control::ReplacementIngressGrant;
use std::collections::VecDeque;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ReplacementIngressError {
    #[error("replacement ingress refused before adoption: {0}")]
    Refused(String),
    #[error("replacement ingress custody remains unconfirmed: {0}")]
    Unconfirmed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct TransferId {
    source: ActorRef,
    sequence: u64,
    target: ActorRef,
}
pub(super) struct TransferCapsule {
    id: TransferId,
    message: KernelMessage,
}
pub(super) struct RejectedCapsules {
    detail: String,
    capsules: Vec<TransferCapsule>,
}
pub(super) enum TransferOutcome {
    Adopted,
    Refused(RejectedCapsules),
    Unconfirmed(String),
}
struct PendingTransfer {
    leases: Vec<AdmittedKernelLease>,
    retained: Vec<TransferCapsule>,
    reply: Option<RpcReplyPort<Result<(), ReplacementIngressError>>>,
}
#[derive(Default)]
pub(super) struct TransferState {
    pending: BTreeMap<u64, PendingTransfer>,
    adopted: BTreeSet<TransferId>,
    adopted_batches: BTreeSet<TransferId>,
    backlog: VecDeque<KernelMessage>,
    relationship: Option<(ActorRef, ActorRef)>,
    activated: bool,
    pub(super) unconfirmed: Option<String>,
}
impl TransferState {
    pub(super) fn has_pending(&self) -> bool {
        !self.pending.is_empty() || !self.backlog.is_empty()
    }
    pub(super) fn pending_count(&self) -> usize {
        self.pending.len()
    }
}
pub(super) enum TransferCommand {
    Begin {
        backlog: VecDeque<KernelMessage>,
        grant: ReplacementIngressGrant,
        reply: RpcReplyPort<Result<(), ReplacementIngressError>>,
    },
    Adopt {
        batch: TransferId,
        grant: ReplacementIngressGrant,
        capsules: Vec<TransferCapsule>,
        reply: RpcReplyPort<Result<(), RejectedCapsules>>,
    },
    Observe {
        batch: TransferId,
        identities: Vec<TransferId>,
        reply: RpcReplyPort<bool>,
    },
    Completed {
        sequence: u64,
        outcome: TransferOutcome,
    },
    Activate {
        grant: ReplacementIngressGrant,
        draining: bool,
        reply: RpcReplyPort<Result<(), ReplacementIngressError>>,
    },
}
impl LifecycleHandle {
    pub(crate) async fn transfer_backlog(
        &self,
        backlog: VecDeque<KernelMessage>,
        grant: ReplacementIngressGrant,
    ) -> Result<(), ReplacementIngressError> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.address
            .cast(LifecycleCommand::Transfer(Box::new(
                TransferCommand::Begin {
                    backlog,
                    grant,
                    reply: reply.into(),
                },
            )))
            .map_err(|error| ReplacementIngressError::Unconfirmed(error.to_string()))?;
        answer.await.map_err(|_| {
            ReplacementIngressError::Unconfirmed(
                "replacement transfer owner ended before settlement".to_owned(),
            )
        })?
    }
    pub(crate) async fn activate_transferred_replacement(
        &self,
        grant: ReplacementIngressGrant,
        draining: bool,
    ) -> Result<(), ReplacementIngressError> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.address
            .cast(LifecycleCommand::Transfer(Box::new(
                TransferCommand::Activate {
                    grant,
                    draining,
                    reply: reply.into(),
                },
            )))
            .map_err(|error| ReplacementIngressError::Unconfirmed(error.to_string()))?;
        answer.await.map_err(|_| {
            ReplacementIngressError::Unconfirmed(
                "replacement activation owner ended before settlement".to_owned(),
            )
        })?
    }
}
pub(super) fn handle(
    state: &mut LifecycleState,
    address: &RactorRef<LifecycleCommand>,
    command: TransferCommand,
) {
    match command {
        TransferCommand::Begin {
            backlog,
            grant,
            reply,
        } => begin(state, address, backlog, grant, reply),
        TransferCommand::Adopt {
            batch,
            grant,
            capsules,
            reply,
        } => {
            let _ = reply.send(adopt(state, address, batch, grant, capsules));
        }
        TransferCommand::Observe {
            batch,
            identities,
            reply,
        } => {
            let _ = reply.send(
                state.transfer.adopted_batches.contains(&batch)
                    && identities
                        .iter()
                        .all(|id| state.transfer.adopted.contains(id)),
            );
        }
        TransferCommand::Completed { sequence, outcome } => {
            let Some(mut pending) = state.transfer.pending.remove(&sequence) else {
                return;
            };
            state.tasks.remove(&sequence);
            match outcome {
                TransferOutcome::Adopted => {
                    // Only target adoption acknowledgment releases the original
                    // counted source entries. Drop settles their exact sequences.
                    pending.leases.clear();
                    if let Some(reply) = pending.reply.take() {
                        let _ = reply.send(Ok(()));
                    }
                }
                TransferOutcome::Refused(rejected) => {
                    if let Some(reply) = pending.reply.take() {
                        let _ = reply.send(Err(ReplacementIngressError::Refused(
                            rejected.detail.clone(),
                        )));
                    }
                    pending.retained = rejected.capsules;
                    state.transfer.unconfirmed = Some(rejected.detail);
                    state.transfer.pending.insert(sequence, pending);
                }
                TransferOutcome::Unconfirmed(detail) => {
                    if let Some(reply) = pending.reply.take() {
                        let _ =
                            reply.send(Err(ReplacementIngressError::Unconfirmed(detail.clone())));
                    }
                    state.transfer.unconfirmed = Some(detail);
                    state.transfer.pending.insert(sequence, pending);
                }
            }
        }
        TransferCommand::Activate {
            grant,
            draining,
            reply,
        } => {
            let _ = reply.send(activate(state, address, grant, draining));
        }
    }
}
fn retain_refused(state: &mut LifecycleState, backlog: VecDeque<KernelMessage>) {
    state.transfer.backlog.extend(backlog);
    state.transfer.unconfirmed = Some("replacement refused before releasing source custody".into());
}
fn begin(
    state: &mut LifecycleState,
    address: &RactorRef<LifecycleCommand>,
    backlog: VecDeque<KernelMessage>,
    grant: ReplacementIngressGrant,
    reply: RpcReplyPort<Result<(), ReplacementIngressError>>,
) {
    let target = grant.successor_handle().clone();
    if grant.predecessor() != state.identity
        || grant.successor() != target.identity
        || state.phase != LifecyclePhase::Closing
        || !state.transfer.pending.is_empty()
    {
        let _ = reply.send(Err(ReplacementIngressError::Refused(
            "replacement transfer does not name this fenced predecessor".into(),
        )));
        retain_refused(state, backlog);
        return;
    }
    if backlog.iter().any(|message| matches!(message, KernelMessage::Admitted(envelope)
        if envelope.identity != state.identity || envelope.lease.identity != state.identity
        || envelope.lease.owner.get_id() != address.get_id() || !state.ingress.contains(&envelope.sequence))) {
        let _ = reply.send(Err(ReplacementIngressError::Refused("replacement backlog contains another admission owner".into())));
        retain_refused(state, backlog); return;
    }
    let sequence = state.next_operation();
    let mut pending = PendingTransfer {
        leases: Vec::new(),
        retained: Vec::new(),
        reply: Some(reply),
    };
    let mut capsules = Vec::with_capacity(backlog.len());
    for message in backlog {
        let (original, message) = match message {
            KernelMessage::Admitted(envelope) => {
                let original = envelope.sequence;
                let (message, lease) = envelope
                    .into_parts(state.identity)
                    .expect("validated original admission");
                pending.leases.push(lease);
                (original, message)
            }
            internal => (state.next_operation(), internal),
        };
        capsules.push(TransferCapsule {
            id: TransferId {
                source: state.identity,
                sequence: original,
                target: target.identity,
            },
            message,
        });
    }
    state.transfer.pending.insert(sequence, pending);
    let source = address.clone();
    let source_identity = state.identity;
    state.tasks.insert(
        sequence,
        tokio::spawn(async move {
            let batch = TransferId {
                source: source_identity,
                sequence,
                target: target.identity,
            };
            let outcome = match AssertUnwindSafe(send_adoption(target, batch, grant, capsules))
                .catch_unwind()
                .await
            {
                Ok(outcome) => outcome,
                Err(_) => {
                    TransferOutcome::Unconfirmed("replacement transfer worker panicked".into())
                }
            };
            let _ = source.cast(LifecycleCommand::Transfer(Box::new(
                TransferCommand::Completed { sequence, outcome },
            )));
        }),
    );
}
async fn send_adoption(
    target: LifecycleHandle,
    batch: TransferId,
    grant: ReplacementIngressGrant,
    capsules: Vec<TransferCapsule>,
) -> TransferOutcome {
    let identities = capsules
        .iter()
        .map(|capsule| capsule.id)
        .collect::<Vec<_>>();
    let (reply, answer) = tokio::sync::oneshot::channel();
    let command = LifecycleCommand::Transfer(Box::new(TransferCommand::Adopt {
        batch,
        grant,
        capsules,
        reply: reply.into(),
    }));
    match target.address.cast(command) {
        Ok(()) => match answer.await {
            Ok(Ok(())) => TransferOutcome::Adopted,
            Ok(Err(rejected)) => TransferOutcome::Refused(rejected),
            Err(_) => {
                // Reobserve the original transfer IDs; never resubmit payloads
                // after an uncertain acknowledgment.
                let (reply, answer) = tokio::sync::oneshot::channel();
                if target
                    .address
                    .cast(LifecycleCommand::Transfer(Box::new(
                        TransferCommand::Observe {
                            batch,
                            identities,
                            reply: reply.into(),
                        },
                    )))
                    .is_ok()
                    && matches!(answer.await, Ok(true))
                {
                    TransferOutcome::Adopted
                } else {
                    TransferOutcome::Unconfirmed(
                        "replacement custody acknowledgment is unavailable".into(),
                    )
                }
            }
        },
        Err(ractor::MessagingErr::SendErr(LifecycleCommand::Transfer(command))) => match *command {
            TransferCommand::Adopt { capsules, .. } => TransferOutcome::Refused(RejectedCapsules {
                detail: "replacement transport refused before target adoption".into(),
                capsules,
            }),
            _ => TransferOutcome::Unconfirmed(
                "replacement transport returned another message".into(),
            ),
        },
        Err(_) => TransferOutcome::Unconfirmed("replacement transport is unavailable".into()),
    }
}
fn adopt(
    state: &mut LifecycleState,
    address: &RactorRef<LifecycleCommand>,
    batch: TransferId,
    grant: ReplacementIngressGrant,
    capsules: Vec<TransferCapsule>,
) -> Result<(), RejectedCapsules> {
    let pair = (grant.predecessor(), grant.successor());
    if batch.source == pair.0
        && batch.target == pair.1
        && state.transfer.adopted_batches.contains(&batch)
    {
        return Ok(());
    }
    if batch.source != pair.0
        || batch.target != pair.1
        || state.is_closed()
        || grant.successor() != state.identity
        || grant.successor_handle().identity != state.identity
        || state
            .transfer
            .relationship
            .is_some_and(|issued| issued != pair)
        || state.transfer.activated
        || capsules
            .iter()
            .any(|capsule| capsule.id.source != pair.0 || capsule.id.target != pair.1)
    {
        return Err(RejectedCapsules {
            detail: "replacement adoption has another staged successor".into(),
            capsules,
        });
    }
    state.transfer.relationship = Some(pair);
    state.transfer.adopted_batches.insert(batch);
    for capsule in capsules {
        if !state.transfer.adopted.insert(capsule.id) {
            continue;
        }
        let sequence = state.next_operation();
        state.ingress.insert(sequence);
        state
            .transfer
            .backlog
            .push_back(KernelMessage::Admitted(AdmittedKernelEnvelope {
                identity: state.identity,
                sequence,
                message: Box::new(capsule.message),
                lease: AdmittedKernelLease {
                    owner: address.clone(),
                    identity: state.identity,
                    sequence,
                },
            }));
    }
    Ok(())
}
fn activate(
    state: &mut LifecycleState,
    address: &RactorRef<LifecycleCommand>,
    grant: ReplacementIngressGrant,
    draining: bool,
) -> Result<(), ReplacementIngressError> {
    if grant.successor() != state.identity
        || state.transfer.relationship != Some((grant.predecessor(), state.identity))
    {
        return Err(ReplacementIngressError::Unconfirmed(
            "replacement activation has another staged relationship".into(),
        ));
    }
    if state.transfer.activated {
        return Ok(());
    }
    if state.is_closed() {
        return Err(ReplacementIngressError::Unconfirmed(
            "replacement successor closed before activation".into(),
        ));
    }
    let Some(executor) = state.executor.clone() else {
        return Err(ReplacementIngressError::Unconfirmed(
            "replacement executor is unavailable".into(),
        ));
    };
    let backlog = std::mem::take(&mut state.transfer.backlog);
    let sequence = state.next_operation();
    state.ingress.insert(sequence);
    let envelope = AdmittedKernelEnvelope {
        identity: state.identity,
        sequence,
        message: Box::new(KernelMessage::ActivateReplacement { backlog, draining }),
        lease: AdmittedKernelLease {
            owner: address.clone(),
            identity: state.identity,
            sequence,
        },
    };
    match executor.cast(KernelMessage::Admitted(envelope)) {
        Ok(()) => {
            state.transfer.activated = true;
            Ok(())
        }
        Err(ractor::MessagingErr::SendErr(KernelMessage::Admitted(envelope))) => {
            let (message, lease) = envelope
                .into_parts(state.identity)
                .expect("own activation envelope");
            drop(lease);
            if let KernelMessage::ActivateReplacement { backlog, .. } = message {
                state.transfer.backlog = backlog;
            }
            state.transfer.unconfirmed =
                Some("replacement executor refused activation transport".into());
            Err(ReplacementIngressError::Unconfirmed(
                "replacement executor refused activation transport".into(),
            ))
        }
        Err(_) => {
            state.transfer.unconfirmed =
                Some("replacement activation transport unavailable".into());
            Err(ReplacementIngressError::Unconfirmed(
                "replacement activation transport unavailable".into(),
            ))
        }
    }
}
