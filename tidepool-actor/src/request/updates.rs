//! Presentation custody for updates to an exact existing request.
use std::sync::Arc;
use std::{fmt, num::NonZeroU64};

use super::{authorize_owner, OwnerState, ReplyError, RequestId, RequestRegistry, TargetState};
use crate::ActorRef;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestUpdateId {
    pub request: RequestId,
    pub sequence: u64,
}

/// Durable delivery identity installed by the host inbox owner before transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestUpdateCorrelation {
    pub producer: String,
    pub sequence: NonZeroU64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LateUpdateEvidence {
    Presented,
    NotPresented(String),
    Unconfirmed(String),
    /// Native evidence was compacted after acknowledgement. Presentation
    /// remains unknown, but further reconciliation is permanently fenced.
    Compacted(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateReconciliationError {
    Reply(ReplyError),
    CorrelationNotInstalled,
    CorrelationMismatch,
    ConflictingTerminalEvidence,
}

impl fmt::Display for UpdateReconciliationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for UpdateReconciliationError {}

#[derive(Debug, Clone, PartialEq, Eq, tidepool_bridge_derive::ToCore)]
pub enum RequestUpdateState {
    #[core(module = "Tidepool.Agent.Reply.Internal")]
    UpdateQueued,
    #[core(module = "Tidepool.Agent.Reply.Internal")]
    UpdatePresented,
    #[core(module = "Tidepool.Agent.Reply.Internal")]
    UpdateTooLate,
    #[core(module = "Tidepool.Agent.Reply.Internal")]
    UpdateUnconfirmed(String),
    #[core(module = "Tidepool.Agent.Reply.Internal")]
    UpdateNotPresented(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum UpdatePhase {
    Queued,
    Presenting,
    Presented,
    TooLate,
    Unconfirmed(String),
    Compacted(String),
    NotPresented(String),
}

pub(super) struct UpdateRecord {
    phase: UpdatePhase,
    correlation: Option<RequestUpdateCorrelation>,
}

impl UpdateRecord {
    pub(super) fn fences_settlement(&self) -> bool {
        matches!(
            self.phase,
            UpdatePhase::Presenting | UpdatePhase::Unconfirmed(_) | UpdatePhase::Compacted(_)
        )
    }
}

/// A queued presentation attempt. Claiming it is atomic with request settlement.
#[derive(Clone)]
pub struct RequestUpdateDelivery {
    registry: Arc<RequestRegistry>,
    owner: ActorRef,
    target: ActorRef,
    id: RequestUpdateId,
    key: String,
    message: String,
}

impl RequestUpdateDelivery {
    pub fn owner(&self) -> ActorRef {
        self.owner
    }

    pub fn id(&self) -> RequestUpdateId {
        self.id
    }

    pub fn target(&self) -> ActorRef {
        self.target
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    /// Install the host's durable inbox identity before any transport attempt.
    pub fn bind_correlation(
        &self,
        correlation: RequestUpdateCorrelation,
    ) -> Result<RequestUpdateReconciler, UpdateReconciliationError> {
        self.registry
            .bind_update_correlation(self.owner, self.id, correlation.clone())?;
        Ok(RequestUpdateReconciler {
            registry: Arc::clone(&self.registry),
            owner: self.owner,
            id: self.id,
            correlation,
        })
    }

    /// At most one claimant can acquire the presentation lease. A reply which
    /// committed first makes the update too late, never a new assignment.
    pub fn begin(self) -> Option<RequestUpdatePresentation> {
        {
            let mut state = self.registry.state.lock();
            let request = state.requests.get_mut(&self.id.request)?;
            let update = request.updates.get_mut(self.id.sequence as usize - 1)?;
            if !matches!(update.phase, UpdatePhase::Queued) {
                return None;
            }
            if !matches!(request.target_state, TargetState::Presented)
                || !matches!(request.owner_state, OwnerState::Observing)
            {
                update.phase = UpdatePhase::TooLate;
                return None;
            }
            update.phase = UpdatePhase::Presenting;
        }
        Some(RequestUpdatePresentation {
            delivery: self,
            finished: false,
        })
    }
}

/// Retained authority to apply late native evidence to one exact update.
#[derive(Clone)]
pub struct RequestUpdateReconciler {
    registry: Arc<RequestRegistry>,
    owner: ActorRef,
    id: RequestUpdateId,
    correlation: RequestUpdateCorrelation,
}

impl RequestUpdateReconciler {
    pub fn id(&self) -> RequestUpdateId {
        self.id
    }

    pub fn correlation(&self) -> &RequestUpdateCorrelation {
        &self.correlation
    }

    pub fn reconcile(&self, evidence: LateUpdateEvidence) -> Result<(), UpdateReconciliationError> {
        self.registry.reconcile_update_presentation(
            self.owner,
            self.id,
            &self.correlation,
            evidence,
        )
    }
}

/// Linear custody of an in-flight presentation. Its existence fences reply
/// settlement and cancellation acknowledgement; requesting cancellation and actor
/// retirement remain possible. Dropping it
/// without backend evidence leaves an explicit unconfirmed state.
#[must_use = "presentation must be confirmed or reported unconfirmed"]
pub struct RequestUpdatePresentation {
    delivery: RequestUpdateDelivery,
    finished: bool,
}

enum PresentationOutcome {
    Presented,
    Unconfirmed(String),
    NotPresented(String),
}

impl RequestUpdatePresentation {
    pub fn key(&self) -> &str {
        &self.delivery.key
    }
    pub fn message(&self) -> &str {
        &self.delivery.message
    }

    /// Call only after the backend confirms insertion into model-visible input.
    /// This certifies presentation, never understanding or incorporation.
    pub fn presented(mut self) {
        self.finish(PresentationOutcome::Presented);
    }

    /// Only valid when the transport has not submitted any input.
    pub fn not_presented(mut self, reason: String) {
        self.finish(PresentationOutcome::NotPresented(reason));
    }

    pub fn unconfirmed(mut self, reason: String) {
        self.finish(PresentationOutcome::Unconfirmed(reason));
    }

    fn finish(&mut self, outcome: PresentationOutcome) {
        // Emit at the custody owner so missing applications, backend errors and
        // dropped presentation tasks all retain the same exact correlation.
        let actor = self.delivery.target;
        let request = self.delivery.id.request;
        let update = self.delivery.id.sequence;
        let update_key = self.key();
        match &outcome {
            PresentationOutcome::Presented => tracing::info!(
                ?actor,
                ?request,
                update,
                update_key,
                "request update presented; incorporation is not confirmed"
            ),
            PresentationOutcome::NotPresented(reason) => tracing::warn!(
                ?actor, ?request, update, update_key, %reason,
                "request update not presented; failure applies to clarification delivery"
            ),
            PresentationOutcome::Unconfirmed(reason) => tracing::warn!(
                ?actor, ?request, update, update_key, %reason,
                "request update presentation unconfirmed; uncertainty fences settlement"
            ),
        }
        let mut state = self.delivery.registry.state.lock();
        if let Some(request) = state.requests.get_mut(&self.delivery.id.request) {
            if let Some(update) = request
                .updates
                .get_mut(self.delivery.id.sequence as usize - 1)
            {
                update.phase = match outcome {
                    PresentationOutcome::Presented => UpdatePhase::Presented,
                    PresentationOutcome::Unconfirmed(reason) => UpdatePhase::Unconfirmed(reason),
                    PresentationOutcome::NotPresented(reason) => UpdatePhase::NotPresented(reason),
                };
            }
        }
        self.finished = true;
    }
}

impl Drop for RequestUpdatePresentation {
    fn drop(&mut self) {
        if !self.finished {
            self.finish(PresentationOutcome::Unconfirmed(
                "presentation task ended without confirmation".into(),
            ));
        }
    }
}

impl RequestRegistry {
    pub(crate) fn update_request(
        self: &Arc<Self>,
        owner: ActorRef,
        id: RequestId,
        message: String,
    ) -> Result<(RequestUpdateId, Option<RequestUpdateDelivery>), ReplyError> {
        let mut state = self.state.lock();
        let request = state.requests.get_mut(&id).ok_or(ReplyError::Stale)?;
        authorize_owner(request, owner)?;
        let queued = match request.target_state {
            TargetState::Presented if matches!(request.owner_state, OwnerState::Observing) => true,
            TargetState::Reserved | TargetState::Queued => return Err(ReplyError::Stale),
            _ => false,
        };
        if queued
            && request.updates.iter().any(|update| {
                matches!(update.phase, UpdatePhase::Queued) || update.fences_settlement()
            })
        {
            return Err(ReplyError::UpdatePending);
        }
        let update = RequestUpdateId {
            request: id,
            sequence: request.updates.len() as u64 + 1,
        };
        request.updates.push(UpdateRecord {
            phase: if queued {
                UpdatePhase::Queued
            } else {
                UpdatePhase::TooLate
            },
            correlation: None,
        });
        let delivery = queued.then(|| RequestUpdateDelivery {
            registry: Arc::clone(self), owner, target: request.target, id: update,
            key: format!("shoal-update-{}", uuid::Uuid::new_v4()),
            message: format!("Update {} for your existing request {}. The original assignment and sessionReply remain pending.\n\n{}", update.sequence, id.0, message),
        });
        Ok((update, delivery))
    }

    pub(crate) fn observe_update(
        &self,
        owner: ActorRef,
        id: RequestUpdateId,
    ) -> Result<RequestUpdateState, ReplyError> {
        let state = self.state.lock();
        let request = state.requests.get(&id.request).ok_or(ReplyError::Stale)?;
        authorize_owner(request, owner)?;
        let index = id.sequence.checked_sub(1).ok_or(ReplyError::Stale)? as usize;
        let update = request.updates.get(index).ok_or(ReplyError::Stale)?;
        Ok(match &update.phase {
            UpdatePhase::Queued
                if !matches!(request.target_state, TargetState::Presented)
                    || !matches!(request.owner_state, OwnerState::Observing) =>
            {
                RequestUpdateState::UpdateTooLate
            }
            UpdatePhase::Queued | UpdatePhase::Presenting => RequestUpdateState::UpdateQueued,
            UpdatePhase::Presented => RequestUpdateState::UpdatePresented,
            UpdatePhase::TooLate => RequestUpdateState::UpdateTooLate,
            UpdatePhase::Unconfirmed(reason) | UpdatePhase::Compacted(reason) => {
                RequestUpdateState::UpdateUnconfirmed(reason.clone())
            }
            UpdatePhase::NotPresented(reason) => {
                RequestUpdateState::UpdateNotPresented(reason.clone())
            }
        })
    }

    /// Bind an update to the exact durable inbox operation before transport.
    fn bind_update_correlation(
        &self,
        owner: ActorRef,
        id: RequestUpdateId,
        correlation: RequestUpdateCorrelation,
    ) -> Result<(), UpdateReconciliationError> {
        let mut state = self.state.lock();
        let request = state
            .requests
            .get_mut(&id.request)
            .ok_or(ReplyError::Stale)
            .map_err(UpdateReconciliationError::Reply)?;
        authorize_owner(request, owner).map_err(UpdateReconciliationError::Reply)?;
        let index = id
            .sequence
            .checked_sub(1)
            .ok_or(ReplyError::Stale)
            .map_err(UpdateReconciliationError::Reply)? as usize;
        let update = request
            .updates
            .get_mut(index)
            .ok_or(ReplyError::Stale)
            .map_err(UpdateReconciliationError::Reply)?;
        match &update.correlation {
            Some(existing) if existing == &correlation => Ok(()),
            Some(_) => Err(UpdateReconciliationError::CorrelationMismatch),
            None if matches!(
                update.phase,
                UpdatePhase::Queued | UpdatePhase::Presenting | UpdatePhase::Unconfirmed(_)
            ) =>
            {
                update.correlation = Some(correlation);
                Ok(())
            }
            None => Err(UpdateReconciliationError::CorrelationNotInstalled),
        }
    }

    /// Apply late authoritative native evidence to the original update only.
    fn reconcile_update_presentation(
        &self,
        owner: ActorRef,
        id: RequestUpdateId,
        correlation: &RequestUpdateCorrelation,
        evidence: LateUpdateEvidence,
    ) -> Result<(), UpdateReconciliationError> {
        let mut state = self.state.lock();
        let request = state
            .requests
            .get_mut(&id.request)
            .ok_or(ReplyError::Stale)
            .map_err(UpdateReconciliationError::Reply)?;
        authorize_owner(request, owner).map_err(UpdateReconciliationError::Reply)?;
        let index = id
            .sequence
            .checked_sub(1)
            .ok_or(ReplyError::Stale)
            .map_err(UpdateReconciliationError::Reply)? as usize;
        let update = request
            .updates
            .get_mut(index)
            .ok_or(ReplyError::Stale)
            .map_err(UpdateReconciliationError::Reply)?;
        match update.correlation.as_ref() {
            None => return Err(UpdateReconciliationError::CorrelationNotInstalled),
            Some(bound) if bound != correlation => {
                return Err(UpdateReconciliationError::CorrelationMismatch)
            }
            Some(_) => {}
        }
        let observed = match evidence {
            LateUpdateEvidence::Presented => UpdatePhase::Presented,
            LateUpdateEvidence::NotPresented(reason) => UpdatePhase::NotPresented(reason),
            LateUpdateEvidence::Unconfirmed(reason) => UpdatePhase::Unconfirmed(reason),
            LateUpdateEvidence::Compacted(reason) => UpdatePhase::Compacted(reason),
        };
        match &update.phase {
            existing if existing == &observed => Ok(()),
            UpdatePhase::Presenting => {
                update.phase = observed;
                Ok(())
            }
            UpdatePhase::Unconfirmed(_)
                if matches!(
                    observed,
                    UpdatePhase::Unconfirmed(_)
                        | UpdatePhase::Presented
                        | UpdatePhase::NotPresented(_)
                        | UpdatePhase::Compacted(_)
                ) =>
            {
                update.phase = observed;
                Ok(())
            }
            UpdatePhase::Presented
            | UpdatePhase::NotPresented(_)
            | UpdatePhase::Unconfirmed(_)
            | UpdatePhase::Compacted(_) => {
                Err(UpdateReconciliationError::ConflictingTerminalEvidence)
            }
            _ => Err(UpdateReconciliationError::CorrelationNotInstalled),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::{CancellationReason, ReplyObservation, ResponseObservation};
    use crate::{ActorId, Incarnation};

    fn active() -> (Arc<RequestRegistry>, ActorRef, ActorRef, RequestId) {
        let registry = Arc::new(RequestRegistry::default());
        let owner = ActorRef::first(ActorId(1));
        let target = ActorRef::first(ActorId(2));
        let request = registry.reserve(owner, target);
        registry.mark_queued(owner, target, request).unwrap();
        registry.present(target, request).unwrap();
        (registry, owner, target, request)
    }

    #[test]
    fn update_authority_is_the_exact_request_owner() {
        let (registry, owner, target, request) = active();
        assert!(matches!(
            registry.update_request(target, request, "tabs".into()),
            Err(ReplyError::Unauthorized)
        ));
        let restarted = ActorRef {
            incarnation: Incarnation(2),
            ..owner
        };
        assert!(matches!(
            registry.update_request(restarted, request, "tabs".into()),
            Err(ReplyError::WrongIncarnation)
        ));
        let (update, _) = registry
            .update_request(owner, request, "tabs".into())
            .unwrap();
        assert_eq!(
            registry.observe_update(target, update),
            Err(ReplyError::Unauthorized)
        );
        assert_eq!(
            registry.observe_update(restarted, update),
            Err(ReplyError::WrongIncarnation)
        );
        assert_eq!(
            registry.observe_update(
                owner,
                RequestUpdateId {
                    sequence: 0,
                    ..update
                }
            ),
            Err(ReplyError::Stale)
        );
    }

    #[test]
    fn presentation_is_single_claim_and_preserves_the_original_response() {
        let (registry, owner, target, request) = active();
        let (update, delivery) = registry
            .update_request(owner, request, "clickable tabs".into())
            .unwrap();
        let delivery = delivery.unwrap();
        let duplicate = delivery.clone();
        let presentation = delivery.begin().unwrap();
        assert!(duplicate.begin().is_none());
        assert_eq!(
            registry.observe_update(owner, update),
            Ok(RequestUpdateState::UpdateQueued)
        );
        assert_eq!(
            registry.begin_reply(target, request),
            Err(ReplyError::UpdatePending)
        );
        assert_eq!(
            registry.observe_response(owner, request),
            Ok(ResponseObservation::Pending)
        );
        presentation.presented();
        assert_eq!(
            registry.observe_update(owner, update),
            Ok(RequestUpdateState::UpdatePresented)
        );
        assert_eq!(
            registry.observe_response(owner, request),
            Ok(ResponseObservation::Pending)
        );
        registry.begin_reply(target, request).unwrap();
        registry.finish_reply(request);
        assert_eq!(
            registry.observe_response(owner, request),
            Ok(ResponseObservation::Ready)
        );
    }

    #[test]
    fn reply_winning_before_claim_makes_update_too_late() {
        let (registry, owner, target, request) = active();
        let (update, delivery) = registry
            .update_request(owner, request, "tabs".into())
            .unwrap();
        registry.begin_reply(target, request).unwrap();
        assert!(delivery.unwrap().begin().is_none());
        assert_eq!(
            registry.observe_update(owner, update),
            Ok(RequestUpdateState::UpdateTooLate)
        );
        registry.finish_reply(request);
        let (late, delivery) = registry
            .update_request(owner, request, "late".into())
            .unwrap();
        assert!(delivery.is_none());
        assert_eq!(
            registry.observe_update(owner, late),
            Ok(RequestUpdateState::UpdateTooLate)
        );
    }

    #[test]
    fn abandoned_presentation_cannot_silently_advance_the_assignment() {
        let (registry, owner, target, request) = active();
        let (update, delivery) = registry
            .update_request(owner, request, "tabs".into())
            .unwrap();
        drop(delivery.unwrap().begin().unwrap());
        assert!(matches!(
            registry.observe_update(owner, update),
            Ok(RequestUpdateState::UpdateUnconfirmed(_))
        ));
        assert_eq!(
            registry.begin_reply(target, request),
            Err(ReplyError::UpdatePending)
        );
        registry
            .cancel_request(owner, request, CancellationReason::RequesterCancelled)
            .unwrap();
        assert_eq!(
            registry.observe_reply(target, request),
            Ok(ReplyObservation::CancellationRequested(
                CancellationReason::RequesterCancelled
            ))
        );
        assert_eq!(
            registry.begin_cancellation_acknowledgement(target, request),
            Err(ReplyError::UpdatePending)
        );
        registry.actor_stopped(
            target,
            &crate::ActorTerminal {
                kind: crate::ActorExitKind::Cancelled,
                summary: "stop after indeterminate delivery".into(),
            },
        );
        assert_eq!(
            registry.observe_reply(target, request),
            Ok(ReplyObservation::Closed)
        );
    }

    #[test]
    fn cancellation_waits_for_in_flight_presentation_but_not_unclaimed_updates() {
        for claim in [false, true] {
            let (registry, owner, target, request) = active();
            let (update, delivery) = registry
                .update_request(owner, request, "tabs".into())
                .unwrap();
            let delivery = delivery.unwrap();
            let presentation = claim.then(|| delivery.clone().begin().unwrap());
            registry
                .cancel_request(owner, request, CancellationReason::RequesterCancelled)
                .unwrap();
            if let Some(presentation) = presentation {
                assert_eq!(
                    registry.begin_cancellation_acknowledgement(target, request),
                    Err(ReplyError::UpdatePending)
                );
                presentation.presented();
                assert_eq!(
                    registry.observe_update(owner, update),
                    Ok(RequestUpdateState::UpdatePresented)
                );
            } else {
                assert!(delivery.begin().is_none());
                assert_eq!(
                    registry.observe_update(owner, update),
                    Ok(RequestUpdateState::UpdateTooLate)
                );
            }
            registry
                .begin_cancellation_acknowledgement(target, request)
                .unwrap();
            registry.finish_cancellation_acknowledgement(request);
            assert_eq!(
                registry.observe_reply(target, request),
                Ok(ReplyObservation::Closed)
            );
        }
    }

    #[test]
    fn failure_before_submission_releases_the_fence() {
        let (registry, owner, target, request) = active();
        let (update, delivery) = registry
            .update_request(owner, request, "tabs".into())
            .unwrap();
        delivery
            .unwrap()
            .begin()
            .unwrap()
            .not_presented("backend unavailable".into());
        assert_eq!(
            registry.observe_update(owner, update),
            Ok(RequestUpdateState::UpdateNotPresented(
                "backend unavailable".into()
            ))
        );
        registry.begin_reply(target, request).unwrap();
    }

    #[test]
    fn only_one_unpresented_update_can_be_outstanding() {
        let (registry, owner, _, request) = active();
        let (_, delivery) = registry
            .update_request(owner, request, "first".into())
            .unwrap();
        assert!(matches!(
            registry.update_request(owner, request, "second".into()),
            Err(ReplyError::UpdatePending)
        ));
        let presentation = delivery.unwrap().begin().unwrap();
        assert!(matches!(
            registry.update_request(owner, request, "second".into()),
            Err(ReplyError::UpdatePending)
        ));
        presentation.presented();
        assert!(registry
            .update_request(owner, request, "second".into())
            .is_ok());
    }

    #[test]
    fn racing_claim_and_reply_have_one_winner() {
        for _ in 0..32 {
            let (registry, owner, target, request) = active();
            let (update, delivery) = registry
                .update_request(owner, request, "tabs".into())
                .unwrap();
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let other = barrier.clone();
            let claimant = std::thread::spawn(move || {
                other.wait();
                delivery.unwrap().begin()
            });
            barrier.wait();
            let reply = registry.begin_reply(target, request);
            match claimant.join().unwrap() {
                Some(presentation) => {
                    assert_eq!(reply, Err(ReplyError::UpdatePending));
                    presentation.presented();
                }
                None => {
                    assert_eq!(reply, Ok(()));
                    assert_eq!(
                        registry.observe_update(owner, update),
                        Ok(RequestUpdateState::UpdateTooLate)
                    );
                }
            }
        }
    }

    #[test]
    fn late_evidence_reconciles_only_the_exact_durable_operation() {
        let (registry, owner, target, request) = active();
        let (update, delivery) = registry
            .update_request(owner, request, "tabs".into())
            .unwrap();
        let correlation = RequestUpdateCorrelation {
            producer: "run-a/inbox-2/actor-2.1".into(),
            sequence: NonZeroU64::new(7).unwrap(),
        };
        let delivery = delivery.unwrap();
        let reconciler = delivery.bind_correlation(correlation.clone()).unwrap();
        assert_eq!(delivery.id(), update);
        assert_eq!(delivery.target(), target);
        delivery
            .begin()
            .unwrap()
            .unconfirmed("lost admission reply".into());

        let foreign_scope = RequestUpdateCorrelation {
            producer: "run-b/inbox-2/actor-2.1".into(),
            ..correlation.clone()
        };
        assert_eq!(
            registry.reconcile_update_presentation(
                owner,
                update,
                &foreign_scope,
                LateUpdateEvidence::Presented,
            ),
            Err(UpdateReconciliationError::CorrelationMismatch)
        );
        assert!(matches!(
            registry.observe_update(owner, update),
            Ok(RequestUpdateState::UpdateUnconfirmed(_))
        ));

        reconciler.reconcile(LateUpdateEvidence::Presented).unwrap();
        assert_eq!(
            registry.observe_update(owner, update),
            Ok(RequestUpdateState::UpdatePresented)
        );
        assert_eq!(
            registry.observe_response(owner, request),
            Ok(ResponseObservation::Pending)
        );
        registry.begin_reply(target, request).unwrap();
    }

    #[test]
    fn repeated_terminal_evidence_is_idempotent_and_conflict_is_retained() {
        let (registry, owner, _, request) = active();
        let (update, delivery) = registry
            .update_request(owner, request, "tabs".into())
            .unwrap();
        let correlation = RequestUpdateCorrelation {
            producer: "run/inbox/actor".into(),
            sequence: NonZeroU64::new(1).unwrap(),
        };
        let delivery = delivery.unwrap();
        let reconciler = delivery.bind_correlation(correlation.clone()).unwrap();
        delivery.begin().unwrap().unconfirmed("timeout".into());
        reconciler
            .reconcile(LateUpdateEvidence::Unconfirmed("timeout".into()))
            .unwrap();
        let evidence = LateUpdateEvidence::NotPresented("withdrawn".into());
        reconciler.reconcile(evidence.clone()).unwrap();
        reconciler.reconcile(evidence).unwrap();
        assert_eq!(
            reconciler.reconcile(LateUpdateEvidence::Presented),
            Err(UpdateReconciliationError::ConflictingTerminalEvidence)
        );
        assert_eq!(
            registry.observe_update(owner, update),
            Ok(RequestUpdateState::UpdateNotPresented("withdrawn".into()))
        );
    }

    #[test]
    fn repeated_unconfirmed_evidence_keeps_the_fence_until_a_terminal_outcome() {
        let (registry, owner, target, request) = active();
        let (update, delivery) = registry
            .update_request(owner, request, "tabs".into())
            .unwrap();
        let correlation = RequestUpdateCorrelation {
            producer: "run/inbox/actor".into(),
            sequence: NonZeroU64::new(1).unwrap(),
        };
        let delivery = delivery.unwrap();
        let reconciler = delivery.bind_correlation(correlation).unwrap();
        delivery
            .begin()
            .unwrap()
            .unconfirmed("lost submit reply".into());

        reconciler
            .reconcile(LateUpdateEvidence::Unconfirmed(
                "later query reply was also lost".into(),
            ))
            .unwrap();
        assert_eq!(
            registry.observe_update(owner, update),
            Ok(RequestUpdateState::UpdateUnconfirmed(
                "later query reply was also lost".into()
            ))
        );
        assert_eq!(
            registry.begin_reply(target, request),
            Err(ReplyError::UpdatePending)
        );

        reconciler.reconcile(LateUpdateEvidence::Presented).unwrap();
        assert_eq!(
            registry.observe_update(owner, update),
            Ok(RequestUpdateState::UpdatePresented)
        );
        registry.begin_reply(target, request).unwrap();
    }

    #[test]
    fn compacted_is_terminal_unconfirmed_and_keeps_settlement_fenced() {
        let (registry, owner, target, request) = active();
        let (update, delivery) = registry
            .update_request(owner, request, "tabs".into())
            .unwrap();
        let correlation = RequestUpdateCorrelation {
            producer: "run/inbox/actor-2.1".into(),
            sequence: NonZeroU64::new(9).unwrap(),
        };
        let delivery = delivery.unwrap();
        let reconciler = delivery.bind_correlation(correlation.clone()).unwrap();
        delivery
            .begin()
            .unwrap()
            .unconfirmed("lost native acknowledgement".into());

        reconciler
            .reconcile(LateUpdateEvidence::Compacted(
                "native evidence compacted".into(),
            ))
            .unwrap();
        assert_eq!(
            registry.observe_update(owner, update),
            Ok(RequestUpdateState::UpdateUnconfirmed(
                "native evidence compacted".into()
            ))
        );
        assert_eq!(
            registry.begin_reply(target, request),
            Err(ReplyError::UpdatePending)
        );
        assert!(matches!(
            registry.update_request(owner, request, "later".into()),
            Err(ReplyError::UpdatePending)
        ));
        assert_eq!(
            reconciler.reconcile(LateUpdateEvidence::Presented),
            Err(UpdateReconciliationError::ConflictingTerminalEvidence)
        );
        let stale = RequestUpdateCorrelation {
            sequence: NonZeroU64::new(10).unwrap(),
            ..correlation
        };
        assert_eq!(
            registry.reconcile_update_presentation(
                owner,
                update,
                &stale,
                LateUpdateEvidence::Compacted("native evidence compacted".into()),
            ),
            Err(UpdateReconciliationError::CorrelationMismatch)
        );
        registry
            .cancel_request(owner, request, CancellationReason::RequesterCancelled)
            .unwrap();
        assert_eq!(
            registry.begin_cancellation_acknowledgement(target, request),
            Err(ReplyError::UpdatePending)
        );
    }
}
