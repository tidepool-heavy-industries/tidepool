//! Request activation's native publication fence. The request record owns its
//! disposition and retained native authority; affine leases supply exact facts.

use super::*;
use std::sync::Arc;
use tidepool_runtime::session::{PublicManifestCommit, PublicationClaim};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NativePhase {
    InFlight,
    OutcomeUnknown,
    PublishedUnconfirmed,
    Published,
    Refused,
}

pub(super) struct ActivationRecord {
    identity: Arc<()>,
    phase: NativePhase,
    retained: Option<Arc<crate::resident_workbench::ActivationPublicationResources>>,
}

impl ActivationRecord {
    pub(super) fn fences_settlement(&mut self) -> bool {
        self.reconcile_confirmation();
        matches!(
            self.phase,
            NativePhase::InFlight | NativePhase::OutcomeUnknown | NativePhase::PublishedUnconfirmed
        )
    }

    fn reconcile_confirmation(&mut self) {
        // Only finish_native's exact visible outcome can enter this state.
        // Native admission prevents any later manifest write while its receipt
        // remains pending, so readiness of this same owner proves confirmation.
        if self.phase == NativePhase::PublishedUnconfirmed
            && self
                .retained
                .as_ref()
                .is_some_and(|resources| resources.confirmed())
        {
            self.phase = NativePhase::Published;
            self.retained = None;
        }
    }
}

pub(crate) struct RequestActivationPublication {
    registry: Arc<RequestRegistry>,
    target: ActorRef,
    request: RequestId,
    identity: Arc<()>,
}

pub(crate) struct RequestActivationCompletion(RequestActivationPublication);

impl RequestRegistry {
    /// Called at the runtime publisher's existing claim point after all native
    /// preflights. Neither this lock nor the actor retirement lock covers fsync.
    pub(crate) fn begin_activation_publication(
        self: &Arc<Self>,
        target: ActorRef,
        request: RequestId,
        retained: Arc<crate::resident_workbench::ActivationPublicationResources>,
        claim: impl FnOnce() -> Option<PublicationClaim>,
    ) -> Result<Option<(PublicationClaim, RequestActivationPublication)>, ReplyError> {
        let mut state = self.state.lock();
        let record = state.requests.get_mut(&request).ok_or(ReplyError::Stale)?;
        authorize_target(record, target)?;
        match record.target_state {
            TargetState::Presented => {}
            TargetState::CancellationRequested { .. }
            | TargetState::AcknowledgingCancellation(_) => {
                return Err(ReplyError::CancellationRequested);
            }
            TargetState::Closed => return Err(ReplyError::AlreadySettled),
            _ => return Err(ReplyError::Stale),
        }
        if record.activation.is_some() {
            return Err(ReplyError::AlreadySettled);
        }
        let Some(claim) = claim() else {
            return Ok(None);
        };
        let identity = Arc::new(());
        record.activation = Some(ActivationRecord {
            identity: identity.clone(),
            phase: NativePhase::InFlight,
            retained: Some(retained),
        });
        Ok(Some((
            claim,
            RequestActivationPublication {
                registry: self.clone(),
                target,
                request,
                identity,
            },
        )))
    }
}

impl RequestActivationPublication {
    fn with_record<T>(&self, operation: impl FnOnce(&mut ActivationRecord) -> T) -> Option<T> {
        let mut state = self.registry.state.lock();
        let record = state.requests.get_mut(&self.request)?;
        if record.target != self.target {
            return None;
        }
        let activation = record.activation.as_mut()?;
        if !Arc::ptr_eq(&activation.identity, &self.identity) {
            return None;
        }
        Some(operation(activation))
    }

    /// The blocking operation settles the real native outcome before its result
    /// can be lost with the async waiter. Known facts survive later lease drop.
    pub(crate) fn finish_native(
        self,
        outcome: &PublicManifestCommit,
    ) -> RequestActivationCompletion {
        self.with_record(|record| {
            if record.phase != NativePhase::InFlight {
                return;
            }
            record.phase = match outcome {
                PublicManifestCommit::Durable | PublicManifestCommit::Ephemeral => {
                    NativePhase::Published
                }
                PublicManifestCommit::PublishedDurabilityUnconfirmed { .. } => {
                    NativePhase::PublishedUnconfirmed
                }
                PublicManifestCommit::BeforeRename { .. }
                | PublicManifestCommit::Cancelled
                | PublicManifestCommit::Stale => NativePhase::Refused,
            };
            if !record.fences_settlement() {
                record.retained = None;
            }
        });
        RequestActivationCompletion(self)
    }
}

impl Drop for RequestActivationPublication {
    fn drop(&mut self) {
        self.with_record(|record| {
            if record.phase == NativePhase::InFlight {
                record.phase = NativePhase::OutcomeUnknown;
            }
        });
    }
}

impl RequestActivationCompletion {
    /// Confirmation runs in its native owning closure, not after JoinHandle delivery.
    pub(crate) fn confirm_native(&self) {
        self.0.with_record(|record| {
            if record.phase == NativePhase::PublishedUnconfirmed {
                record.phase = NativePhase::Published;
                record.retained = None;
            }
        });
    }

    pub(crate) fn publish_if_current<R>(
        self,
        publish: impl FnOnce() -> R,
    ) -> Result<R, ReplyError> {
        let state = self.0.registry.state.lock();
        let record = state
            .requests
            .get(&self.0.request)
            .ok_or(ReplyError::Stale)?;
        authorize_target(record, self.0.target)?;
        if record.activation.as_ref().is_none_or(|activation| {
            !Arc::ptr_eq(&activation.identity, &self.0.identity)
                || activation.phase != NativePhase::Published
        }) {
            return Err(ReplyError::Stale);
        }
        match record.target_state {
            TargetState::Presented => Ok(publish()),
            TargetState::CancellationRequested { .. }
            | TargetState::AcknowledgingCancellation(_) => Err(ReplyError::CancellationRequested),
            TargetState::Closed => Err(ReplyError::AlreadySettled),
            _ => Err(ReplyError::Stale),
        }
    }
}
