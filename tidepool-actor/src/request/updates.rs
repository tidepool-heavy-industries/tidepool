//! Presentation custody for updates to an exact existing request.
use std::sync::Arc;

use super::{authorize_owner, OwnerState, ReplyError, RequestId, RequestRegistry, TargetState};
use crate::ActorRef;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestUpdateId {
    pub request: RequestId,
    pub sequence: u64,
}

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

pub(super) enum UpdateRecord {
    Queued,
    Presenting,
    Presented,
    TooLate,
    Unconfirmed(String),
    NotPresented(String),
}

impl UpdateRecord {
    pub(super) fn fences_settlement(&self) -> bool {
        matches!(self, Self::Presenting | Self::Unconfirmed(_))
    }
}

/// A queued presentation attempt. Claiming it is atomic with request settlement.
#[derive(Clone)]
pub struct RequestUpdateDelivery {
    registry: Arc<RequestRegistry>,
    target: ActorRef,
    id: RequestUpdateId,
    key: String,
    message: String,
}

impl RequestUpdateDelivery {
    pub fn target(&self) -> ActorRef {
        self.target
    }

    /// At most one claimant can acquire the presentation lease. A reply which
    /// committed first makes the update too late, never a new assignment.
    pub fn begin(self) -> Option<RequestUpdatePresentation> {
        {
            let mut state = self.registry.state.lock();
            let request = state.requests.get_mut(&self.id.request)?;
            let update = request.updates.get_mut(self.id.sequence as usize - 1)?;
            if !matches!(update, UpdateRecord::Queued) {
                return None;
            }
            if !matches!(request.target_state, TargetState::Presented)
                || !matches!(request.owner_state, OwnerState::Observing)
            {
                *update = UpdateRecord::TooLate;
                return None;
            }
            *update = UpdateRecord::Presenting;
        }
        Some(RequestUpdatePresentation {
            delivery: self,
            finished: false,
        })
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
                *update = match outcome {
                    PresentationOutcome::Presented => UpdateRecord::Presented,
                    PresentationOutcome::Unconfirmed(reason) => UpdateRecord::Unconfirmed(reason),
                    PresentationOutcome::NotPresented(reason) => UpdateRecord::NotPresented(reason),
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
            && request
                .updates
                .iter()
                .any(|update| matches!(update, UpdateRecord::Queued) || update.fences_settlement())
        {
            return Err(ReplyError::UpdatePending);
        }
        let update = RequestUpdateId {
            request: id,
            sequence: request.updates.len() as u64 + 1,
        };
        request.updates.push(if queued {
            UpdateRecord::Queued
        } else {
            UpdateRecord::TooLate
        });
        let delivery = queued.then(|| RequestUpdateDelivery {
            registry: Arc::clone(self), target: request.target, id: update,
            key: format!("shoal-update-{}", uuid::Uuid::new_v4()),
            message: format!("Update {} for your existing request {}. The original assignment and sessionReply remain pending.\n\n{}\n\nPresentation is not incorporation. Explain your intended response and provide task-specific evidence when available.", update.sequence, id.0, message),
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
        Ok(match update {
            UpdateRecord::Queued
                if !matches!(request.target_state, TargetState::Presented)
                    || !matches!(request.owner_state, OwnerState::Observing) =>
            {
                RequestUpdateState::UpdateTooLate
            }
            UpdateRecord::Queued | UpdateRecord::Presenting => RequestUpdateState::UpdateQueued,
            UpdateRecord::Presented => RequestUpdateState::UpdatePresented,
            UpdateRecord::TooLate => RequestUpdateState::UpdateTooLate,
            UpdateRecord::Unconfirmed(reason) => {
                RequestUpdateState::UpdateUnconfirmed(reason.clone())
            }
            UpdateRecord::NotPresented(reason) => {
                RequestUpdateState::UpdateNotPresented(reason.clone())
            }
        })
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
}
