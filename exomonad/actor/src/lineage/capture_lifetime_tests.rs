use super::*;
use tidepool_runtime::session::PersistentSession;

fn drain_dropped_capsules(session: &mut PersistentSession) {
    // A normal runtime admission entry drains its owner's retirement queue.
    let temporary = session.retain_lexical_scope(ScopeId::ROOT).unwrap();
    let scope = temporary.scope();
    drop(temporary);
    session.retire_scope(scope);
}

fn capture(
    groups: &ForkGroupRegistry,
    session: &mut PersistentSession,
) -> (String, ScopeId, ScopeId, WorkbenchForkBoundary) {
    let parent = session.mint_isolated_scope();
    let original = session.mint_detached_scope(parent).unwrap();
    let retained = session.retain_lexical_scope(original).unwrap();
    let retained_scope = retained.scope();
    let boundary = WorkbenchForkBoundary::external(
        "thread".into(),
        "request".into(),
        "unfinished-parent".into(),
    );
    let token = groups.capture_checkpoint_with_retained_scope(
        "capture".into(),
        ActorRef::first(crate::ActorId(1)),
        crate::ActorCapabilities::default(),
        None,
        None,
        crate::CheckpointSourceLayer::default(),
        SessionId(7),
        original,
        boundary.clone(),
        None,
        retained,
        crate::ActorPersistencePolicy::Durable,
    );
    (token, original, retained_scope, boundary)
}

fn claim_capture(groups: &ForkGroupRegistry, token: &str, name: &str) -> CheckpointLease {
    let owner = ActorRef::first(crate::ActorId(1));
    let (group, paths) = groups
        .begin(
            owner,
            ActorPath::parse(name).unwrap(),
            vec![ActorPathSegment::new("child").unwrap()],
            None,
        )
        .unwrap();
    groups
        .claim_with_checkpoint(
            group,
            owner,
            &paths[0].allocated,
            Some((token, SessionId(7))),
        )
        .unwrap()
        .checkpoint
        .unwrap()
        .0
}

#[test]
fn released_capture_preserves_two_admissions_until_last_lexical_share() {
    let groups = ForkGroupRegistry::new(ActorLineageRegistry::default());
    let mut session = PersistentSession::new(None, 1024);
    let (token, original, retained_scope, boundary) = capture(&groups, &mut session);
    groups
        .settle_checkpoint(&token, SessionId(7), true)
        .unwrap();
    let first = claim_capture(&groups, &token, "first");
    let second = claim_capture(&groups, &token, "second");
    assert_eq!(
        first.issuer_persistence_policy,
        crate::ActorPersistencePolicy::Durable
    );
    assert_eq!(
        second.issuer_persistence_policy,
        crate::ActorPersistencePolicy::Durable
    );
    // Both admissions won while the parent's enclosing execution is unfinished.
    assert!(groups
        .settle_checkpoints(first.issuer, &boundary, false)
        .is_empty());
    groups.retire_actor(first.issuer);
    assert_eq!(
        groups.release_checkpoint(&token, SessionId(7)),
        Ok(Some(original))
    );
    session.retire_scope(original);
    groups
        .confirm_checkpoint_release(&token, SessionId(7), original)
        .unwrap();
    assert!(
        groups.retains_session(SessionId(7)),
        "in-flight admissions retain the machine"
    );
    assert!(matches!(
        groups.preview_checkpoint(&token, SessionId(7)),
        Err(CheckpointRefusal::ReleasedCheckpoint)
    ));
    // Original-root retirement happened before either asynchronous admission
    // resumed. They mint from their actual capsules, never the dead scope ID.
    let first_child = session
        .mint_scope_from_lease(first.retained_scope().unwrap())
        .unwrap();
    let second_child = session
        .mint_scope_from_lease(second.retained_scope().unwrap())
        .unwrap();
    drop(first);
    drain_dropped_capsules(&mut session);
    assert!(session.scope_tree().is_live(retained_scope));
    drop(second);
    drain_dropped_capsules(&mut session);
    assert!(!session.scope_tree().is_live(retained_scope));
    assert!(!groups.retains_session(SessionId(7)));
    assert!(session.scope_tree().is_live(first_child));
    assert!(session.scope_tree().is_live(second_child));
    session.retire_scope(first_child);
    session.retire_scope(second_child);
}

#[test]
fn undelivered_capture_releases_its_runtime_capsule() {
    let groups = ForkGroupRegistry::new(ActorLineageRegistry::default());
    let mut session = PersistentSession::new(None, 1024);
    let (token, original, retained_scope, boundary) = capture(&groups, &mut session);
    assert_eq!(
        groups.settle_checkpoints(ActorRef::first(crate::ActorId(1)), &boundary, false),
        vec![(SessionId(7), original)]
    );
    session.retire_scope(original);
    drain_dropped_capsules(&mut session);
    assert!(!session.scope_tree().is_live(retained_scope));
    assert!(!groups.retains_session(SessionId(7)));
    assert!(matches!(
        groups.checkpoint(&token, SessionId(7)),
        Err(CheckpointRefusal::CaptureFailed)
    ));
}

#[test]
fn retained_checkpoint_scope_refuses_another_runtime_owner() {
    let groups = ForkGroupRegistry::new(ActorLineageRegistry::default());
    let mut source = PersistentSession::new(None, 1024);
    let (token, _, _, _) = capture(&groups, &mut source);
    groups
        .settle_checkpoint(&token, SessionId(7), true)
        .unwrap();
    let admitted = claim_capture(&groups, &token, "foreign-owner");
    let mut foreign = PersistentSession::new(None, 1024);
    assert!(foreign
        .mint_scope_from_lease(admitted.retained_scope().unwrap())
        .is_err());
}
