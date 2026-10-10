//! Admission, incorporation and retirement through genuine compiler-issued roots.

use super::*;
use crate::ActorId;

fn actor(id: u64) -> ActorRef {
    ActorRef::first(ActorId(id))
}

fn presented(registry: &RequestRegistry, owner: ActorRef, target: ActorRef) -> RequestId {
    let request = registry.reserve_native(owner, target);
    registry.mark_queued(owner, target, request).unwrap();
    registry.present(target, request).unwrap();
    request
}

fn stopped() -> ActorTerminal {
    ActorTerminal {
        kind: ActorExitKind::Completed,
        summary: String::new(),
        diagnostic: None,
    }
}

#[test]
fn readiness_without_an_admitted_native_destination_is_refused() {
    let registry = RequestRegistry::default();
    let owner = actor(1);
    let target = actor(2);
    let request = registry.reserve_for_operation(owner, target, "unadmitted".into(), true, None);
    registry.mark_queued(owner, target, request).unwrap();
    registry.present(target, request).unwrap();
    assert_eq!(
        registry.begin_reply(target, request),
        Err(ReplyError::ReplyResultUnavailable)
    );
    assert!(matches!(
        registry.observe_response(owner, request),
        Ok(ResponseObservation::Pending(_))
    ));
    assert!(matches!(
        registry.observe_response_result(owner, request),
        Err(ReplyError::ReplyResultUnavailable)
    ));
}

#[test]
fn selected_root_survives_forget_and_private_read_then_releases_native_custody() {
    use readiness::{Node, Plan};
    let registry = RequestRegistry::default();
    let owner = actor(1);
    let target = actor(2);
    let baseline = test_support::outstanding_custody();
    let request = presented(&registry, owner, target);
    let source = registry.register_watch(owner, vec![request]).unwrap().0;
    let claim = registry.begin_reply(target, request).unwrap();
    let admission = Arc::downgrade(&claim.destination());
    let root = test_support::snapshot(&claim);
    let retained = Arc::downgrade(&root);
    let session = root.session();
    registry.finish_reply(claim, root.clone(), None);
    assert!(
        admission.upgrade().is_none(),
        "terminal success must release the admission lease"
    );
    assert_eq!(test_support::outstanding_custody(), baseline + 1);
    registry.forget_response(owner, request).unwrap();
    let observed =
        |source| Plan::checked(vec![Node::Leaf(ReadinessDependency::Watch(source))], 0).unwrap();
    let nested = registry
        .register_watch_plan(owner, "nested".into(), observed(source))
        .unwrap()
        .0;
    let read = registry
        .register_transient_watch(owner, observed(nested))
        .unwrap();
    registry.forget_watch(owner, source).unwrap();
    registry.forget_watch(owner, nested).unwrap();
    registry.actor_stopped(target, &stopped());
    registry.actor_stopped(owner, &stopped());
    drop(root);
    for _ in 0..3 {
        let snapshot = registry
            .observe_watch_snapshot_response(read, &[0, 0], 0)
            .unwrap();
        assert_eq!(snapshot.session(), session);
        assert_eq!(test_support::force(&snapshot), 41);
    }
    assert!(retained.upgrade().is_some());
    registry.release_transient_watch(owner, read).unwrap();
    assert!(retained.upgrade().is_none());
    assert_eq!(test_support::outstanding_custody(), baseline);
}

#[test]
fn incorporation_rechecks_target_retirement_and_drops_the_provisional_root() {
    let registry = RequestRegistry::default();
    let owner = actor(1);
    let target = actor(2);
    let baseline = test_support::outstanding_custody();
    let request = presented(&registry, owner, target);
    let claim = registry.begin_reply(target, request).unwrap();
    let admission = Arc::downgrade(&claim.destination());
    let root = test_support::snapshot(&claim);
    let retained = Arc::downgrade(&root);
    registry.actor_stopped(target, &stopped());
    assert!(registry.finish_reply(claim, root, None).is_empty());
    assert!(admission.upgrade().is_none());
    assert!(retained.upgrade().is_none());
    assert_eq!(
        registry.observe_response(owner, request),
        Ok(ResponseObservation::Unavailable(
            ResponseFailure::TargetUnavailable
        ))
    );
    assert_eq!(test_support::outstanding_custody(), baseline);
}

#[test]
fn owner_retirement_keeps_native_settlement_obligation_and_releases_inflight_pin() {
    let registry = RequestRegistry::default();
    let owner = actor(1);
    let target = actor(2);
    let baseline = test_support::outstanding_custody();
    let request = presented(&registry, owner, target);
    registry.actor_stopped(owner, &stopped());
    let claim = registry.begin_reply(target, request).unwrap();
    let admission = Arc::downgrade(&claim.destination());
    let root = test_support::snapshot(&claim);
    let retained = Arc::downgrade(&root);
    registry.actor_stopped(owner, &stopped());
    registry.finish_reply(claim, root, None);
    assert!(admission.upgrade().is_none());
    assert!(retained.upgrade().is_none());
    assert_eq!(
        registry.request_cleanup_state(owner, request),
        Ok(RequestCleanupState::TargetClosed)
    );
    assert_eq!(
        registry.observe_response(owner, request),
        Ok(ResponseObservation::Unavailable(
            ResponseFailure::RequesterStopped
        ))
    );
    assert_eq!(test_support::outstanding_custody(), baseline);
}

#[test]
fn a_different_admission_cannot_settle_an_accepted_reply() {
    let registry = RequestRegistry::default();
    let owner = actor(1);
    let target = actor(2);
    let first = presented(&registry, owner, target);
    let second = presented(&registry, owner, target);
    let first_claim = registry.begin_reply(target, first).unwrap();
    let second_claim = registry.begin_reply(target, second).unwrap();
    let second_root = test_support::snapshot(&second_claim);
    registry.finish_reply(first_claim, second_root.clone(), None);
    assert!(matches!(
        registry.observe_response(owner, first),
        Ok(ResponseObservation::Unavailable(
            ResponseFailure::SettlementFailed(_)
        ))
    ));
    assert_eq!(
        registry.request_cleanup_state(owner, first),
        Ok(RequestCleanupState::TargetClosed)
    );
    registry.finish_reply(second_claim, second_root, None);
    assert_eq!(
        test_support::force(&registry.observe_response_result(owner, second).unwrap()),
        41
    );
}

#[test]
fn cleanup_transfer_preserves_the_exact_issued_destination() {
    let registry = RequestRegistry::default();
    let owner = actor(1);
    let target = actor(2);
    let request = registry.reserve_for_cleanup_owner(
        owner,
        target,
        "transfer".into(),
        true,
        None,
        ResourceCleanupOwner::Scope(7),
    );
    test_support::admit_destination(&registry, owner, request);
    let original = registry.state.lock().requests[&request]
        .result_destination
        .clone()
        .unwrap();
    registry
        .transfer_request_cleanup_owner(
            owner,
            request,
            &ResourceCleanupOwner::Scope(7),
            ResourceCleanupOwner::Actor,
        )
        .unwrap();
    registry.mark_queued(owner, target, request).unwrap();
    registry.present(target, request).unwrap();
    let claim = registry.begin_reply(target, request).unwrap();
    assert!(Arc::ptr_eq(&original, &claim.destination()));
    let snapshot = test_support::snapshot(&claim);
    assert_eq!(snapshot.session(), original.session());
    assert_eq!(snapshot.type_witness(), original.type_witness());
    registry.finish_reply(claim, snapshot, None);
    assert_eq!(
        test_support::force(&registry.observe_response_result(owner, request).unwrap()),
        41
    );
}
