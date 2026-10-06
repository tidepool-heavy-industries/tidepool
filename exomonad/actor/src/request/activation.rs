//! Request settlement retains the native input owner until its original
//! publication decision and native confirmation state establish an outcome.

#[cfg(test)]
mod phase_properties;

use super::*;
use std::sync::Arc;
use tidepool_runtime::session::{PublicationClaim, PublicationPhase};

pub(super) enum ActivationRecord {
    Pending(Arc<crate::resident_workbench::ActivationPublicationResources>),
    Published,
    Refused,
}

impl ActivationRecord {
    fn reconcile(&mut self) {
        let Self::Pending(resources) = self else {
            return;
        };
        match resources.phase() {
            PublicationPhase::Published if resources.confirmed() => *self = Self::Published,
            PublicationPhase::Terminated => *self = Self::Refused,
            // CommitClaimed stays fenced even when the native graph is ready:
            // only the original native publisher can establish local promotion.
            _ => {}
        }
    }

    pub(super) fn fences_settlement(&mut self) -> bool {
        self.reconcile();
        matches!(self, Self::Pending(_))
    }
}

pub(crate) enum ActivationPublicationRefusal {
    Request(ReplyError),
    Retired(ActorTerminal),
}

pub(crate) struct RequestActivationPublication {
    registry: Arc<RequestRegistry>,
    target: ActorRef,
    request: RequestId,
}

pub(crate) struct RequestActivationCompletion(RequestActivationPublication);

impl RequestRegistry {
    /// The native publisher calls this after its ticket, epoch and view checks.
    /// Only the retained input's original decision can supply the native claim;
    /// neither request nor retirement locks cover fsync or local promotion.
    pub(crate) fn begin_activation_publication(
        self: &Arc<Self>,
        target: ActorRef,
        request: RequestId,
        retained: Arc<crate::resident_workbench::ActivationPublicationResources>,
        retirement: &crate::RetainedActorExit,
    ) -> Result<
        Option<(PublicationClaim, RequestActivationPublication)>,
        ActivationPublicationRefusal,
    > {
        let mut state = self.state.lock();
        let record = state
            .requests
            .get_mut(&request)
            .ok_or(ActivationPublicationRefusal::Request(ReplyError::Stale))?;
        authorize_target(record, target).map_err(ActivationPublicationRefusal::Request)?;
        if !retained.authorizes(target) {
            return Err(ActivationPublicationRefusal::Request(
                ReplyError::Unauthorized,
            ));
        }
        match record.target_state {
            TargetState::Presented => {}
            TargetState::CancellationRequested { .. }
            | TargetState::AcknowledgingCancellation(_) => {
                return Err(ActivationPublicationRefusal::Request(
                    ReplyError::CancellationRequested,
                ))
            }
            TargetState::Closed => {
                return Err(ActivationPublicationRefusal::Request(
                    ReplyError::AlreadySettled,
                ))
            }
            _ => return Err(ActivationPublicationRefusal::Request(ReplyError::Stale)),
        }
        if record.activation.is_some() {
            return Err(ActivationPublicationRefusal::Request(
                ReplyError::AlreadySettled,
            ));
        }
        let mut claim = None;
        retirement
            .claim_before_shutdown(|| {
                claim = retained.claim_commit();
                claim.is_some()
            })
            .map_err(ActivationPublicationRefusal::Retired)?;
        let Some(claim) = claim else {
            return Ok(None);
        };
        record.activation = Some(ActivationRecord::Pending(retained));
        Ok(Some((
            claim,
            RequestActivationPublication {
                registry: self.clone(),
                target,
                request,
            },
        )))
    }
}

impl RequestActivationPublication {
    fn reconcile(&self) {
        let mut state = self.registry.state.lock();
        if let Some(record) = state.requests.get_mut(&self.request) {
            if record.target == self.target {
                if let Some(activation) = &mut record.activation {
                    activation.reconcile();
                }
            }
        }
    }

    /// Native reconciliation precedes delivery to the async waiter. Drop also
    /// reads the same authoritative facts and cannot manufacture an outcome.
    pub(crate) fn finish(self) -> RequestActivationCompletion {
        self.reconcile();
        RequestActivationCompletion(self)
    }
}

impl Drop for RequestActivationPublication {
    fn drop(&mut self) {
        self.reconcile();
    }
}

impl RequestActivationCompletion {
    pub(crate) fn publish_if_current<R>(
        self,
        publish: impl FnOnce() -> R,
    ) -> Result<R, ReplyError> {
        let mut state = self.0.registry.state.lock();
        let record = state
            .requests
            .get_mut(&self.0.request)
            .ok_or(ReplyError::Stale)?;
        authorize_target(record, self.0.target)?;
        if let Some(activation) = &mut record.activation {
            activation.reconcile();
        }
        if !matches!(record.activation, Some(ActivationRecord::Published)) {
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
