use super::{
    authorize_owner, CancellationReason, ReplyError, RequestId, RequestRegistry,
    RequestReservationOwner, ResourceCleanupOwner, TargetState,
};
use crate::ActorRef;
use serde::{Deserialize, Serialize};

/// Target-side cleanup evidence, independent of the requester's released wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum RequestCleanupState {
    Active,
    ReplySettling,
    CancellationRequested {
        reason: CancellationReason,
        presented: bool,
    },
    AcknowledgingCancellation(CancellationReason),
    TargetClosed,
}

impl RequestRegistry {
    /// Transfer an exact invocation's own request to actor lifetime. Keep the
    /// reservation origin so a detached, unsubmitted request still rolls back.
    pub(crate) fn detach_invocation_request(
        &self,
        owner: ActorRef,
        request: RequestId,
        reservation: Option<&RequestReservationOwner>,
    ) -> Result<(), ReplyError> {
        let mut state = self.state.lock();
        let record = state.requests.get_mut(&request).ok_or(ReplyError::Stale)?;
        authorize_owner(record, owner)?;
        if record.command_job.is_some() {
            return Err(ReplyError::Unauthorized);
        }
        if let Some(reservation) = reservation {
            if record.reservation_owner.as_ref() != Some(reservation)
                || !matches!(reservation, RequestReservationOwner::Workbench { .. })
            {
                return Err(ReplyError::Unauthorized);
            }
            match &record.cleanup_owner {
                ResourceCleanupOwner::Invocation(current) if current == reservation => {
                    record.cleanup_owner = ResourceCleanupOwner::Actor;
                }
                ResourceCleanupOwner::Actor => {}
                _ => return Err(ReplyError::Unauthorized),
            }
        } else if record.cleanup_owner != ResourceCleanupOwner::Actor {
            return Err(ReplyError::Unauthorized);
        }
        // A serialized record handler has no hosted invocation. Its existing
        // actor-owned request needs no transfer.
        Ok(())
    }

    /// Retain terminal rows as well: the invocation's cleanup receipt must
    /// preserve completed replies and actual target closure evidence.
    pub(crate) fn invocation_requests(
        &self,
        owner: ActorRef,
        reservation: &RequestReservationOwner,
    ) -> Vec<RequestId> {
        if !matches!(reservation, RequestReservationOwner::Workbench { .. }) {
            return Vec::new();
        }
        self.cleanup_owner_requests(
            owner,
            &ResourceCleanupOwner::Invocation(reservation.clone()),
        )
    }

    /// Include terminal rows so cleanup can retain actual target closure
    /// evidence. Command settlement rows belong to their command's owner.
    pub(crate) fn cleanup_owner_requests(
        &self,
        owner: ActorRef,
        cleanup_owner: &ResourceCleanupOwner,
    ) -> Vec<RequestId> {
        let state = self.state.lock();
        let mut requests = state
            .requests
            .iter()
            .filter_map(|(request, record)| {
                (record.owner == owner
                    && &record.cleanup_owner == cleanup_owner
                    && record.command_job.is_none())
                .then_some(*request)
            })
            .collect::<Vec<_>>();
        requests.sort_unstable();
        requests
    }

    pub(crate) fn request_cleanup_owner(
        &self,
        owner: ActorRef,
        request: RequestId,
    ) -> Result<ResourceCleanupOwner, ReplyError> {
        let state = self.state.lock();
        let record = state.requests.get(&request).ok_or(ReplyError::Stale)?;
        authorize_owner(record, owner)?;
        if record.command_job.is_some() {
            return Err(ReplyError::Unauthorized);
        }
        Ok(record.cleanup_owner.clone())
    }

    /// The source and destination resource owners must fence closure while
    /// this comparison and transfer runs. Actor authority and construction
    /// provenance do not move with cleanup membership.
    pub(crate) fn transfer_request_cleanup_owner(
        &self,
        owner: ActorRef,
        request: RequestId,
        expected: &ResourceCleanupOwner,
        destination: ResourceCleanupOwner,
    ) -> Result<(), ReplyError> {
        let mut state = self.state.lock();
        let record = state.requests.get_mut(&request).ok_or(ReplyError::Stale)?;
        authorize_owner(record, owner)?;
        if record.command_job.is_some() || &record.cleanup_owner != expected {
            return Err(ReplyError::Unauthorized);
        }
        record.cleanup_owner = destination;
        Ok(())
    }

    pub(crate) fn request_cleanup_state(
        &self,
        owner: ActorRef,
        request: RequestId,
    ) -> Result<RequestCleanupState, ReplyError> {
        let state = self.state.lock();
        let record = state.requests.get(&request).ok_or(ReplyError::Stale)?;
        authorize_owner(record, owner)?;
        Ok(match record.target_state {
            TargetState::Reserved | TargetState::Queued | TargetState::Presented => {
                RequestCleanupState::Active
            }
            TargetState::Settling => RequestCleanupState::ReplySettling,
            TargetState::CancellationRequested { reason, presented } => {
                RequestCleanupState::CancellationRequested { reason, presented }
            }
            TargetState::AcknowledgingCancellation(reason) => {
                RequestCleanupState::AcknowledgingCancellation(reason)
            }
            TargetState::Closed => RequestCleanupState::TargetClosed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::{
        CancelRequestOutcome, ResponseFailure, ResponseObservation, WatchId,
        WorkbenchReservationAttempt,
    };
    use crate::{ActorId, Incarnation};

    fn actor(id: u64) -> ActorRef {
        ActorRef::first(ActorId(id))
    }

    fn invocation(digest: u8) -> RequestReservationOwner {
        RequestReservationOwner::Workbench {
            execution: tidepool_runtime::session::WorkbenchExecutionId::from_digest([digest; 16]),
            attempt: WorkbenchReservationAttempt::fresh(),
        }
    }

    fn reserve(
        registry: &RequestRegistry,
        owner: ActorRef,
        target: ActorRef,
        invocation: &RequestReservationOwner,
    ) -> RequestId {
        let request = registry.reserve_for_operation(
            owner,
            target,
            "invocation request".into(),
            true,
            Some(invocation.clone()),
        );
        crate::request::test_support::admit_destination(registry, owner, request);
        request
    }

    #[test]
    fn detach_requires_exact_owner_incarnation_execution_and_attempt() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let scope = invocation(1);
        let retry = invocation(1);
        let sibling = invocation(2);
        let request = reserve(&registry, owner, actor(2), &scope);
        assert_eq!(registry.invocation_requests(owner, &scope), vec![request]);
        assert!(registry.invocation_requests(owner, &retry).is_empty());
        for other_scope in [&retry, &sibling] {
            assert_eq!(
                registry.detach_invocation_request(owner, request, Some(other_scope)),
                Err(ReplyError::Unauthorized)
            );
        }
        assert_eq!(
            registry.detach_invocation_request(actor(3), request, Some(&scope)),
            Err(ReplyError::Unauthorized)
        );
        let restarted = ActorRef {
            id: owner.id,
            incarnation: Incarnation(2),
        };
        assert_eq!(
            registry.detach_invocation_request(restarted, request, Some(&scope)),
            Err(ReplyError::WrongIncarnation)
        );
        assert_eq!(
            registry.detach_invocation_request(owner, request, Some(&scope)),
            Ok(())
        );
        assert_eq!(
            registry.detach_invocation_request(owner, request, Some(&scope)),
            Ok(())
        );
        assert!(registry.invocation_requests(owner, &scope).is_empty());
        assert_eq!(
            registry.detach_invocation_request(owner, request, Some(&retry)),
            Err(ReplyError::Unauthorized)
        );
    }

    #[test]
    fn detached_reservation_preserves_original_abort_fence() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let scope = invocation(1);
        let retry = invocation(1);
        let request = reserve(&registry, owner, actor(2), &scope);
        registry
            .detach_invocation_request(owner, request, Some(&scope))
            .unwrap();
        assert!(registry.abort_unsubmitted(owner, &retry).0.is_empty());
        assert_eq!(registry.abort_unsubmitted(owner, &scope).0, vec![request]);
        assert_eq!(
            registry.detach_invocation_request(owner, request, Some(&scope)),
            Err(ReplyError::Stale)
        );
    }

    #[test]
    fn explicit_cleanup_lifetime_does_not_change_construction_provenance() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let construction = invocation(1);
        let retry = invocation(1);
        for cleanup_owner in [
            ResourceCleanupOwner::Actor,
            ResourceCleanupOwner::Scope(9),
            ResourceCleanupOwner::Run,
        ] {
            let request = registry.reserve_for_cleanup_owner(
                owner,
                actor(2),
                "request".into(),
                true,
                Some(construction.clone()),
                cleanup_owner.clone(),
            );
            assert!(registry
                .invocation_requests(owner, &construction)
                .is_empty());
            assert_eq!(
                registry.cleanup_owner_requests(owner, &cleanup_owner),
                vec![request]
            );
            assert!(registry.abort_unsubmitted(owner, &retry).0.is_empty());
            assert_eq!(
                registry.abort_unsubmitted(owner, &construction).0,
                vec![request]
            );
        }
    }

    #[test]
    fn cleanup_transfer_preserves_authority_and_abort_fence() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let construction = invocation(1);
        let source = ResourceCleanupOwner::Scope(3);
        let parent = ResourceCleanupOwner::Scope(2);
        let request = registry.reserve_for_cleanup_owner(
            owner,
            actor(2),
            "scope request".into(),
            true,
            Some(construction.clone()),
            source.clone(),
        );
        assert_eq!(
            registry.transfer_request_cleanup_owner(actor(2), request, &source, parent.clone()),
            Err(ReplyError::Unauthorized)
        );
        assert_eq!(
            registry.transfer_request_cleanup_owner(
                ActorRef {
                    id: owner.id,
                    incarnation: Incarnation(2)
                },
                request,
                &source,
                parent.clone(),
            ),
            Err(ReplyError::WrongIncarnation)
        );
        assert_eq!(
            registry.transfer_request_cleanup_owner(owner, request, &source, parent.clone()),
            Ok(())
        );
        assert!(registry.cleanup_owner_requests(owner, &source).is_empty());
        assert_eq!(
            registry.cleanup_owner_requests(owner, &parent),
            vec![request]
        );
        assert_eq!(
            registry.transfer_request_cleanup_owner(
                owner,
                request,
                &source,
                ResourceCleanupOwner::Actor
            ),
            Err(ReplyError::Unauthorized)
        );
        assert_eq!(
            registry.request_cleanup_owner(owner, request),
            Ok(parent.clone())
        );
        assert_eq!(
            registry.detach_invocation_request(owner, request, Some(&construction)),
            Err(ReplyError::Unauthorized)
        );
        assert_eq!(
            registry.detach_invocation_request(owner, request, None),
            Err(ReplyError::Unauthorized)
        );
        assert_eq!(
            registry.transfer_request_cleanup_owner(
                owner,
                request,
                &parent,
                ResourceCleanupOwner::Actor
            ),
            Ok(())
        );
        assert_eq!(
            registry.abort_unsubmitted(owner, &construction).0,
            vec![request]
        );
    }

    #[test]
    fn cleanup_owner_selection_cannot_transfer_command_settlements() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let request = registry.reserve_command_settlement(owner, "job".into(), true);
        assert!(registry
            .cleanup_owner_requests(owner, &ResourceCleanupOwner::Actor)
            .is_empty());
        assert_eq!(
            registry.transfer_request_cleanup_owner(
                owner,
                request,
                &ResourceCleanupOwner::Actor,
                ResourceCleanupOwner::Scope(1)
            ),
            Err(ReplyError::Unauthorized)
        );
        assert_eq!(
            registry.request_cleanup_owner(owner, request),
            Err(ReplyError::Unauthorized)
        );
    }

    #[test]
    fn run_cleanup_rows_survive_requester_metadata_retirement() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let target = actor(2);
        let request = registry.reserve_for_cleanup_owner(
            owner,
            target,
            "run request".into(),
            true,
            None,
            ResourceCleanupOwner::Run,
        );
        registry.mark_queued(owner, target, request).unwrap();
        assert_eq!(registry.forget_terminal_actor_metadata(owner), Ok(vec![]));
        assert_eq!(
            registry.cleanup_owner_requests(owner, &ResourceCleanupOwner::Run),
            vec![request]
        );
        assert_eq!(
            registry.request_cleanup_state(owner, request),
            Ok(RequestCleanupState::Active)
        );
        assert!(registry.forget_terminal_actor_metadata(target).is_err());
    }

    #[test]
    fn record_handler_requests_keep_actor_lifetime_and_routes_keep_rollback() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let scope = invocation(1);
        let request = registry.reserve(owner, actor(2));
        assert_eq!(
            registry.detach_invocation_request(owner, request, None),
            Ok(())
        );
        assert_eq!(
            registry.detach_invocation_request(actor(3), request, None),
            Err(ReplyError::Unauthorized)
        );
        assert_eq!(
            registry.detach_invocation_request(owner, request, Some(&scope)),
            Err(ReplyError::Unauthorized)
        );
        let route = RequestReservationOwner::Route(WatchId(1));
        let route_request = reserve(&registry, owner, actor(2), &route);
        assert!(registry.invocation_requests(owner, &route).is_empty());
        assert_eq!(
            registry.detach_invocation_request(owner, route_request, None),
            Ok(())
        );
        assert_eq!(
            registry.detach_invocation_request(owner, route_request, Some(&route)),
            Err(ReplyError::Unauthorized)
        );
        assert_eq!(
            registry.abort_unsubmitted(owner, &route).0,
            vec![route_request]
        );
    }

    #[test]
    fn invocation_cleanup_preserves_completed_reply_and_detached_submitted_request() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let target = actor(2);
        let scope = invocation(1);
        let ready = reserve(&registry, owner, target, &scope);
        let detached = reserve(&registry, owner, target, &scope);
        registry.mark_queued(owner, target, ready).unwrap();
        registry.mark_queued(owner, target, detached).unwrap();
        registry
            .detach_invocation_request(owner, detached, Some(&scope))
            .unwrap();
        registry.present(target, ready).unwrap();
        let mut reply_claim_ready = Some(registry.begin_reply(target, ready).unwrap());
        assert_eq!(
            registry.request_cleanup_state(owner, ready),
            Ok(RequestCleanupState::ReplySettling)
        );
        crate::request::test_support::complete_optional_reply(
            &registry,
            &mut reply_claim_ready,
            None,
        );
        assert_eq!(registry.invocation_requests(owner, &scope), vec![ready]);
        assert_eq!(
            registry.cancel_request(owner, ready, CancellationReason::RequesterCancelled),
            Ok((CancelRequestOutcome::AlreadyTerminal, None))
        );
        assert_eq!(
            registry.observe_response(owner, ready),
            Ok(ResponseObservation::Ready)
        );
        assert_eq!(
            registry.request_cleanup_state(owner, ready),
            Ok(RequestCleanupState::TargetClosed)
        );
        assert!(registry.abort_unsubmitted(owner, &scope).0.is_empty());
        assert!(matches!(
            registry.observe_response(owner, detached),
            Ok(ResponseObservation::Pending(_))
        ));
    }

    #[test]
    fn abandoned_wait_still_cancels_actual_target_and_retains_acknowledgment_uncertainty() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let target = actor(2);
        let scope = invocation(1);
        let request = reserve(&registry, owner, target, &scope);
        registry.mark_queued(owner, target, request).unwrap();
        registry.present(target, request).unwrap();
        registry.abandon_response(owner, request).unwrap();
        let (outcome, notification) = registry
            .cancel_request(owner, request, CancellationReason::RequesterCancelled)
            .unwrap();
        assert_eq!(outcome, CancelRequestOutcome::Requested);
        assert_eq!(notification.unwrap().target, target);
        assert_eq!(
            registry.request_cleanup_state(owner, request),
            Ok(RequestCleanupState::CancellationRequested {
                reason: CancellationReason::RequesterCancelled,
                presented: true,
            })
        );
        registry
            .begin_cancellation_acknowledgement(target, request)
            .unwrap();
        assert_eq!(
            registry.request_cleanup_state(owner, request),
            Ok(RequestCleanupState::AcknowledgingCancellation(
                CancellationReason::RequesterCancelled
            ))
        );
        registry.rollback_cancellation_acknowledgement(request);
        assert_eq!(
            registry.cancel_request(owner, request, CancellationReason::RequesterCancelled),
            Ok((CancelRequestOutcome::AlreadyRequested, None))
        );
        registry
            .begin_cancellation_acknowledgement(target, request)
            .unwrap();
        registry.finish_cancellation_acknowledgement(request);
        assert_eq!(
            registry.request_cleanup_state(owner, request),
            Ok(RequestCleanupState::TargetClosed)
        );
        let next = registry.reserve(owner, target);
        registry.mark_queued(owner, target, next).unwrap();
        assert_eq!(registry.present(target, next), Ok(None));
    }

    #[test]
    fn deadline_wait_release_is_not_target_cleanup_and_inspection_checks_owner() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let target = actor(2);
        let request = reserve(&registry, owner, target, &invocation(1));
        registry.mark_queued(owner, target, request).unwrap();
        registry.present(target, request).unwrap();
        registry.deadline_request(owner, request);
        assert_eq!(
            registry.observe_response(owner, request),
            Ok(ResponseObservation::Unavailable(
                ResponseFailure::DeadlineExceeded
            ))
        );
        assert_eq!(
            registry.cancel_request(owner, request, CancellationReason::RequesterCancelled),
            Ok((CancelRequestOutcome::AlreadyRequested, None))
        );
        assert_eq!(
            registry.request_cleanup_state(owner, request),
            Ok(RequestCleanupState::CancellationRequested {
                reason: CancellationReason::DeadlineExpired,
                presented: true,
            })
        );
        assert_eq!(
            registry.request_cleanup_state(target, request),
            Err(ReplyError::Unauthorized)
        );
        assert_eq!(
            registry.request_cleanup_state(
                ActorRef {
                    id: owner.id,
                    incarnation: Incarnation(2)
                },
                request
            ),
            Err(ReplyError::WrongIncarnation)
        );
    }
}
